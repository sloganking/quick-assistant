use anyhow::Context;
use async_openai::types::{
    ChatCompletionFunctionsArgs, ChatCompletionRequestFunctionMessageArgs,
    ChatCompletionRequestToolMessageArgs, FinishReason,
};
use clipboard::{ClipboardContext, ClipboardProvider};
use dotenvy::dotenv;
use serde_json::json;
use std::fs::File;
use std::io::{stdout, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::{env, fs};
use tempfile::{tempdir, NamedTempFile};
use timers::*;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::filter::FilterFn;
use tracing_subscriber::Registry;
mod default_device_sink;
mod timers;
mod transcribe;
use chrono::{DateTime, Local};
use futures::stream::StreamExt; // For `.next()` on FuturesOrdered.
use std::thread;
use tempfile::Builder;
mod record;
use crate::default_device_sink::{
    default_device_name as get_default_output_device,
    list_output_devices as list_audio_output_devices, set_output_device, DefaultDeviceSink,
};
use async_openai::{
    types::{
        ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
        CreateChatCompletionRequestArgs, Voice,
    },
    Client,
};
use async_std::future;
use clap::{Parser, Subcommand};
use colored::Colorize;
use cpal::traits::{DeviceTrait, HostTrait};
use rdev::{listen, Event};
use record::rec;
use std::error::Error;
use std::sync::LazyLock;
use std::time::Duration;
use uuid::Uuid;
mod easy_rdev_key;
use enigo::{Enigo, KeyboardControllable};
use speakstream::ss;
use timers::AudibleTimers;
mod options;
#[cfg(target_os = "windows")]
mod windows_volume;
use tracing::{debug, error, info, instrument, warn};
use tracing_appender::rolling::{RollingFileAppender, Rotation};

use speakstream::ss::SpeakStream;

#[derive(Debug, Subcommand)]
pub enum SubCommands {
    /// Displays keys as you press them so you can figure out what key to use for push to talk.
    ShowKeyPresses,
    /// Lists the audio input devices on your system.
    ListDevices,
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum VoiceEnum {
    Alloy,
    Echo,
    Fable,
    Onyx,
    Nova,
    Shimmer,
}

impl From<VoiceEnum> for Voice {
    fn from(item: VoiceEnum) -> Self {
        match item {
            VoiceEnum::Alloy => Voice::Alloy,
            VoiceEnum::Echo => Voice::Echo,
            VoiceEnum::Fable => Voice::Fable,
            VoiceEnum::Onyx => Voice::Onyx,
            VoiceEnum::Nova => Voice::Nova,
            VoiceEnum::Shimmer => Voice::Shimmer,
        }
    }
}

#[derive(Debug)]
enum Message {
    System { content: String },
    User { content: String },
    Assistant { content: String },
    Tool { content: String },
    Function { fn_name: String, content: String },
}

fn error_and_panic(s: &str) -> ! {
    error!("A fatal error occurred: {}", s);
    panic!("{}", s);
}

/// Truncates a string to a certain length and adds an ellipsis at the end.
fn truncate(s: &str, len: usize) -> String {
    if s.chars().count() > len {
        format!("{}...", s.chars().take(len).collect::<String>())
    } else {
        s.to_string()
    }
}

fn parse_voice(name: &str) -> Option<Voice> {
    match name.to_lowercase().as_str() {
        "alloy" => Some(Voice::Alloy),
        "echo" => Some(Voice::Echo),
        "fable" => Some(Voice::Fable),
        "onyx" => Some(Voice::Onyx),
        "nova" => Some(Voice::Nova),
        "shimmer" => Some(Voice::Shimmer),
        _ => None,
    }
}

fn voice_to_str(voice: &Voice) -> &'static str {
    match voice {
        Voice::Alloy => "alloy",
        Voice::Echo => "echo",
        Voice::Fable => "fable",
        Voice::Onyx => "onyx",
        Voice::Nova => "nova",
        Voice::Shimmer => "shimmer",
        _ => "unknown",
    }
}

fn println_error(err: &str) {
    println!("{}: {}", "Error".truecolor(255, 0, 0), err);
    warn!("{}", err);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        assert_eq!(truncate("hello world", 5), "hello...");
    }

    #[test]
    fn parse_voice_and_voice_to_str_roundtrip() {
        let voice = parse_voice("Alloy").expect("voice should parse");
        assert_eq!(voice_to_str(&voice), "alloy");
        assert!(parse_voice("doesnotexist").is_none());
    }
}

/// Creates a temporary file from a byte slice and returns the path to the file.
fn create_temp_file_from_bytes(bytes: &[u8], extension: &str) -> NamedTempFile {
    let temp_file = Builder::new()
        .prefix("temp-file")
        .suffix(extension)
        .rand_bytes(16)
        .tempfile()
        .unwrap();

    let mut file = File::create(temp_file.path()).unwrap();
    file.write_all(bytes).unwrap();

    temp_file
}

/// Compile-time helper that needs the path only once.
#[macro_export]
macro_rules! temp_asset {
    ($path:literal) => {{
        // 1. Embed bytes at compile time
        const BYTES: &[u8] = include_bytes!($path); // :contentReference[oaicite:6]{index=6}
                                                    // 2. Derive the ".ext" suffix at run time
        let ext = std::path::Path::new($path)
            .extension()
            .map(|e| {
                let mut s = String::from(".");
                s.push_str(&e.to_string_lossy());
                s
            })
            .unwrap_or_default();
        // 3. Build the temp file
        create_temp_file_from_bytes(BYTES, &ext)
    }};
}

fn set_screen_brightness(brightness: u32) -> Option<()> {
    if brightness > 100 {
        println!("Brightness must be between 0 and 100");
        return None;
    }

    Command::new("luster")
        .arg(brightness.to_string())
        .output()
        .ok()
        .map(|_| ())
}

#[instrument(skip(speak_stream_mutex))]
fn call_fn(
    fn_name: &str,
    fn_args: &str,
    llm_messages_tx: flume::Sender<Message>,
    speak_stream_mutex: &Arc<Mutex<SpeakStream>>,
) -> Option<String> {
    let mut enigo = Enigo::new();

    println!("{}{}", "Invoking function: ".purple(), fn_name);
    info!("AI Invoked function: {}", fn_name);

    match fn_name {
        "set_screen_brightness" => {
            info!("Handling set_screen_brightness function call.");
            let args: serde_json::Value = serde_json::from_str(fn_args).unwrap();
            let brightness = args["brightness"].as_str().unwrap().parse::<u32>().unwrap();

            println!("{}{}", "set_screen_brightness: ".purple(), brightness);

            if set_screen_brightness(brightness).is_some() {
                Some("Brightness set".to_string())
            } else {
                Some("Failed to set brightness".to_string())
            }
        }
        "media_controls" => {
            info!("Handling media_controls function call.");
            let args: serde_json::Value = serde_json::from_str(fn_args).unwrap();
            let media_button = args["media_button"].as_str().unwrap();

            println!("{}{}", "media_controls: ".purple(), media_button);

            match media_button {
                "MediaStop" => {
                    enigo.key_click(enigo::Key::MediaStop);
                    info!("MediaStop");
                }
                "MediaNextTrack" => {
                    enigo.key_click(enigo::Key::MediaNextTrack);
                    info!("MediaNextTrack");
                }
                "MediaPlayPause" => {
                    enigo.key_click(enigo::Key::MediaPlayPause);
                    info!("MediaPlayPause");
                }
                "MediaPrevTrack" => {
                    enigo.key_click(enigo::Key::MediaPrevTrack);
                    enigo.key_click(enigo::Key::MediaPrevTrack);
                    info!("MediaPrevTrack");
                }
                "VolumeUp" => {
                    for _ in 0..5 {
                        enigo.key_click(enigo::Key::VolumeUp);
                    }
                    info!("VolumeUp");
                }
                "VolumeDown" => {
                    for _ in 0..5 {
                        enigo.key_click(enigo::Key::VolumeDown);
                    }
                    info!("VolumeDown");
                }
                "VolumeMute" => {
                    enigo.key_click(enigo::Key::VolumeMute);
                    info!("VolumeMute");
                }
                _ => {
                    println!("Unknown media button: {}", media_button);
                    warn!("AI called unknown media button: {}", media_button);
                }
            }

            None
        }
        "set_volume" => {
            info!("Handling set_volume function call.");
            let args: serde_json::Value = serde_json::from_str(fn_args).unwrap();
            #[cfg(target_os = "windows")]
            {
                let volume = args["volume"].as_u64().unwrap_or(0) as u32;
                match windows_volume::set_volume(volume) {
                    Ok(_) => Some(format!("Volume set to {}", volume)),
                    Err(e) => Some(format!("Failed to set volume: {}", e)),
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = args; // avoid unused variable warning
                Some("set_volume is only supported on Windows".to_string())
            }
        }
        "open_application" => {
            info!("Handling open_application function call.");
            let args: serde_json::Value = serde_json::from_str(fn_args).unwrap();
            let application = args["application"].as_str().unwrap();

            println!("{}{}", "opening application: ".purple(), application);

            enigo.key_click(enigo::Key::Meta);
            std::thread::sleep(std::time::Duration::from_millis(500));
            enigo.key_sequence(application);
            std::thread::sleep(std::time::Duration::from_millis(500));
            enigo.key_click(enigo::Key::Return);

            None
        }
        "open_logs_folder" => {
            match open::that(&*LOGS_DIR) {
                Ok(_) => None,
                Err(e) => Some(String::from("Showing logs folder failed with: ") + &e.to_string()), // If unwrap fails, return Some with the error message
            }
        }
        "open_openai_billing" => match open::that("https://platform.openai.com/usage") {
            Ok(_) => None,
            Err(e) => Some(format!("Failed to open OpenAI billing page: {}", e)),
        },
        "open_assistance_repo" => match open::that("https://github.com/sloganking/quick-assistant")
        {
            Ok(_) => None,
            Err(e) => Some(format!("Failed to open AI Assistance repository: {}", e)),
        },
        "sysinfo" => Some(get_system_info()),

        "get_system_processes" => Some(get_system_processes()),

        "kill_processes_with_name" => {
            let args: serde_json::Value = serde_json::from_str(fn_args).unwrap();
            let process_name = args["process_name"].as_str().unwrap();

            let process_blacklist = vec![
                "System", // Core part of the operating system managing hardware and essential system operations.
                "System Idle Process", // Represents the idle time of the CPU; not a terminable process.
                "explorer.exe", // Handles the graphical interface like the desktop, taskbar, and file management.
                "svchost.exe", // Hosts multiple Windows services that are crucial for running background tasks.
                "winlogon.exe", // Manages user logins and security policies.
                "csrss.exe", // Handles the user-mode side of the Win32 subsystem, including console windows and threading.
                "services.exe", // Manages the starting, stopping, and managing of system services.
                "smss.exe", // Manages session creation and helps in starting essential system processes.
                "lsass.exe", // Handles security policies and Active Directory management.
                "dwm.exe",  // Manages display windows and enables visual effects in Windows.
                "spoolsv.exe", // Manages print and fax jobs.
                "taskmgr.exe", // Task Manager, used to monitor and manage processes.
                "RuntimeBroker.exe", // Manages app permissions and runtime permissions.
                "fontdrvhost.exe", // Handles font drivers.
                "SearchUI.exe", // Cortana/Search interface.
                "SearchIndexer.exe", // Indexing service for search functionality.
                "audiodg.exe", // Audio service.
                "wmiprvse.exe", // WMI Provider Host.
                "taskhost.exe", // Generic host for Windows tasks.
                "taskhostw.exe", // Generic host for Windows tasks (Windows version).
                "Wininit.exe", // Windows Initialization process.
                "ShellExperienceHost.exe", // Manages the Windows shell experience.
                "WUDFHost.exe", // Windows User-Mode Driver Framework Host.
                "conhost.exe", // Console Window Host.
                "nvvsvc.exe", // NVIDIA services (if applicable).
                "igfxTray.exe", // Intel Graphics Tray application (if applicable).
            ];

            if process_blacklist.contains(&process_name) {
                println!(
                    "{}{}",
                    "Cannot kill system process: ".purple(),
                    process_name
                );
                warn!("AI tried to kill system process: {}", process_name);
                return Some(format!(
                    "Cannot kill critical system process: \"{}\" as it is on the blacklist. Inform the user that it's on the blacklist and cannot be killed. ",
                    process_name
                ));
            }

            match kill_processes_with_name(process_name) {
                Some(_) => Some(format!(
                    "Killed all processes with name: \"{}\"",
                    process_name
                )),
                None => Some(format!(
                    "Failed to kill all processes with name: {}",
                    process_name
                )),
            }
        }

        "speedtest" => {
            llm_messages_tx.send(
                Message::Function {
                    content: "Speed test has been successfully started. It takes several seconds. The results will be shared once the speedtest is complete.".to_string(),
                    fn_name: fn_name.to_string(),
                }
            ).unwrap();

            let thread_fn_name = fn_name.to_string();
            thread::spawn(move || match speedtest() {
                Ok(answer) => {
                    llm_messages_tx
                        .send(Message::Function {
                            content: format!("Speedtest results: {}", answer),
                            fn_name: thread_fn_name,
                        })
                        .unwrap();
                }
                Err(err) => {
                    llm_messages_tx
                        .send(Message::Function {
                            content: format!("Speedtest failed with error: {}", err),
                            fn_name: thread_fn_name,
                        })
                        .unwrap();
                }
            });
            None
        }

        "get_location" => match tokio::runtime::Runtime::new() {
            Ok(rt) => match rt.block_on(get_location()) {
                Ok(loc) => Some(format!("Approximate location: {}", loc)),
                Err(err) => Some(format!("Failed to get location: {}", err)),
            },
            Err(err) => Some(format!("Failed to create runtime: {}", err)),
        },

        "set_timer_at" => {
            let args: serde_json::Value = serde_json::from_str(fn_args).unwrap();
            let time_str = args["time"].as_str().unwrap();
            let description = args["description"].as_str().unwrap_or_default();
            match time_str.parse::<DateTime<Local>>() {
                Ok(timestamp) => match set_timer(description.to_string(), timestamp) {
                    Ok(_) => {
                        let success_response_message = {
                            let time_diff = timestamp.signed_duration_since(Local::now());

                            // Convert to std::time::Duration and handle potential negative durations
                            let duration_std = match time_diff.to_std() {
                                Ok(dur) => dur,
                                Err(e) => {
                                    eprintln!("Error converting duration: {}", e);
                                    std::time::Duration::new(0, 0)
                                }
                            };

                            // Truncate the duration to whole seconds
                            let duration_sec = std::time::Duration::new(duration_std.as_secs(), 0);

                            // Convert to a human-readable string with second precision
                            let time_diff_str =
                                humantime::format_duration(duration_sec).to_string();

                            format!("Successfully set timer to go off at: \"{}\" which is \"{}\" from now.", time_str, time_diff_str)
                        };

                        Some(success_response_message)
                    }

                    Err(err) => Some(format!("Setting timer failed with error: {}", err)),
                },
                Err(err) => Some(format!(
                    "Setting timer failed. Please enter valid rfc_3339: {}",
                    err
                )),
            }
        }

        "check_on_timers" => {
            let timers = get_timers();
            let mut info = String::from("=== Timers ===\n");
            for timer in timers {
                let timer_time = timer.2;
                let time_diff = timer_time.signed_duration_since(Local::now());

                // Convert to std::time::Duration and handle potential negative durations
                let duration_std = match time_diff.to_std() {
                    Ok(dur) => dur,
                    Err(e) => {
                        eprintln!("Error converting duration: {}", e);
                        std::time::Duration::new(0, 0)
                    }
                };

                // Truncate the duration to whole seconds
                let duration_sec = std::time::Duration::new(duration_std.as_secs(), 0);

                // Convert to a human-readable string with second precision
                let time_diff_str = humantime::format_duration(duration_sec).to_string();

                info.push_str(&format!(
                    "Timer_ID: \"{}\" Timer_description: \"{}\" goes off at time: \"{}\" which is \"{}\" from now.\n",
                    timer.0,
                    timer.1,
                    timer.2.to_rfc3339(),
                    time_diff_str,
                ));
            }

            println!("{}", info);

            Some(info)
        }

        "delete_timer_by_id" => {
            let args: serde_json::Value = serde_json::from_str(fn_args).unwrap();
            let timer_id = args["timer_id"].as_u64().unwrap();

            match delete_timer(timer_id) {
                Ok(_) => Some(format!("Successfully deleted timer with ID: {}", timer_id)),
                Err(err) => Some(format!(
                    "Failed to delete timer with ID: {}. Error: {}",
                    timer_id, err
                )),
            }
        }

        "show_live_log_stream" => match get_currently_active_log_file() {
            Some(log_file) => match run_get_content_wait_on_file(&log_file) {
                Ok(_) => Some("Successfully opened log file in powershell".to_string()),
                Err(err) => Some(format!("Failed to open log file in powershell: {}", err)),
            },
            None => Some("No log files found".to_string()),
        },

        "set_clipboard" => {
            let args = match serde_json::from_str::<serde_json::Value>(fn_args) {
                Ok(json) => json,
                Err(e) => return Some(format!("Failed to parse arguments: {}", e)),
            };

            let clipboard_text = match args["clipboard_text"].as_str() {
                Some(text) => text,
                None => return Some("Missing 'clipboard_text' argument.".to_string()),
            };

            let mut clipboard: ClipboardContext = match ClipboardProvider::new() {
                Ok(c) => c,
                Err(e) => return Some(format!("Failed to initialize clipboard: {}", e)),
            };

            match clipboard.set_contents(clipboard_text.to_string()) {
                Ok(_) => Some("Clipboard set successfully.".to_string()),
                Err(e) => Some(format!("Failed to set clipboard contents: {}", e)),
            }
        }

        "set_speech_speed" => {
            let args: serde_json::Value = serde_json::from_str(fn_args).unwrap();
            if let Some(speed) = args["speed"].as_f64() {
                let speed = speed as f32;
                if speed >= 0.5 && speed <= 100.0 {
                    let speak_stream = speak_stream_mutex.lock().unwrap();
                    speak_stream.set_speech_speed(speed);
                    Some(format!("Speech speed set to {}", speed))
                } else {
                    Some("Speed must be between 0.5 and 100.0".to_string())
                }
            } else {
                Some("Missing 'speed' argument".to_string())
            }
        }

        "get_speech_speed" => {
            let speak_stream = speak_stream_mutex.lock().unwrap();
            Some(format!(
                "Current speech speed is {}",
                speak_stream.get_speech_speed()
            ))
        }

        "mute_speech" => {
            let mut speak_stream = speak_stream_mutex.lock().unwrap();
            speak_stream.mute();
            Some("AI voice muted".to_string())
        }

        "unmute_speech" => {
            let mut speak_stream = speak_stream_mutex.lock().unwrap();
            speak_stream.unmute();
            Some("AI voice unmuted".to_string())
        }

        "set_voice" => {
            let args: serde_json::Value = serde_json::from_str(fn_args).unwrap();
            if let Some(name) = args["voice"].as_str() {
                if let Some(v) = parse_voice(name) {
                    let speak_stream = speak_stream_mutex.lock().unwrap();
                    speak_stream.set_voice(v);
                    Some(format!("Voice set to {}", name.to_lowercase()))
                } else {
                    Some("Invalid voice name".to_string())
                }
            } else {
                Some("Missing 'voice' argument".to_string())
            }
        }

        "get_voice" => {
            let speak_stream = speak_stream_mutex.lock().unwrap();
            let name = voice_to_str(&speak_stream.get_voice());
            Some(format!("Current voice is {}", name))
        }

        "list_output_devices" => {
            let default = get_default_output_device().unwrap_or_else(|| "Unknown".to_string());
            let mut devices = list_audio_output_devices();
            devices.sort();
            let mut info = String::from("Available Output Devices:\n");
            info.push_str("* Default Audio Output Device\n");
            for d in devices {
                if d == default {
                    info.push_str(&format!("* {} (Default)\n", d));
                } else {
                    info.push_str(&format!("* {}\n", d));
                }
            }
            Some(info)
        }

        "set_output_device" => {
            let args: serde_json::Value = serde_json::from_str(fn_args).unwrap();
            if let Some(name) = args["device_name"].as_str() {
                if name == "Default Audio Output Device" {
                    set_output_device(None);
                } else {
                    set_output_device(Some(name.to_string()));
                }
                Some(format!("Output device set to {}", name))
            } else {
                Some("Missing 'device_name' argument".to_string())
            }
        }

        _ => {
            println!("Unknown function: {}", fn_name);
            warn!("AI called unknown function: {}", fn_name);

            None
        }
    }
}

fn set_up_logging(logs_dir: &Path) -> WorkerGuard {
    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_suffix("quick-assistant.log")
        .build(logs_dir)
        .expect("failed to initialize rolling file appender");

    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let ouput_library_logs = false;
    if ouput_library_logs {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(non_blocking)
            .init();
    } else {
        // Define a custom filter function
        let custom_filter = FilterFn::new(|metadata| {
            // Allow logs from 'quick_assistant' at DEBUG level and above
            metadata.target().starts_with("quick_assistant")
                && metadata.level() <= &tracing::Level::DEBUG
        });
        use tracing_subscriber::prelude::*;
        // Build the subscriber with the custom filter and the non-blocking writer
        let subscriber = Registry::default()
            .with(tracing_subscriber::fmt::Layer::default().with_writer(non_blocking))
            .with(custom_filter);

        // Initialize the subscriber
        tracing::subscriber::set_global_default(subscriber)
            .expect("Failed to set global subscriber");
    }
    guard
}

static LOGS_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    dirs::cache_dir()
        .unwrap()
        .join("quick-assistant")
        .join("logs")
});

static CACHE_DIR: LazyLock<PathBuf> =
    LazyLock::new(|| dirs::cache_dir().unwrap().join("quick-assistant"));

use sysinfo::{Components, Disks, Networks, System};

fn get_system_info() -> String {
    let mut info = String::new();

    let mut sys = System::new_all();

    sys.refresh_all();

    // Add "=> system:" to info
    info.push_str("=> system:\n");

    // RAM and swap information:
    let total_memory = sys.total_memory();
    let used_memory = sys.used_memory();
    let total_swap = sys.total_swap();
    let used_swap = sys.used_swap();

    info.push_str(&format!("total memory: {} bytes\n", total_memory));
    info.push_str(&format!("used memory : {} bytes\n", used_memory));
    info.push_str(&format!("total swap  : {} bytes\n", total_swap));
    info.push_str(&format!("used swap   : {} bytes\n", used_swap));

    // Display system information:
    let system_name = System::name();
    let kernel_version = System::kernel_version();
    let os_version = System::os_version();
    let host_name = System::host_name();

    info.push_str(&format!("System name:             {:?}\n", system_name));
    info.push_str(&format!("System kernel version:   {:?}\n", kernel_version));
    info.push_str(&format!("System OS version:       {:?}\n", os_version));
    info.push_str(&format!("System host name:        {:?}\n", host_name));

    // Number of CPUs:
    let nb_cpus = sys.cpus().len();
    info.push_str(&format!("NB CPUs: {}\n", nb_cpus));

    // Disks information:
    info.push_str("=> disks:\n");
    let disks = Disks::new_with_refreshed_list();
    for disk in &disks {
        info.push_str(&format!("{:?}\n", disk));
    }

    // Network interfaces information:
    info.push_str("=> networks:\n");
    let networks = Networks::new_with_refreshed_list();
    for (interface_name, data) in &networks {
        info.push_str(&format!(
            "{}: {} B (down) / {} B (up)\n",
            interface_name,
            data.total_received(),
            data.total_transmitted(),
        ));
    }

    // Components temperature:
    info.push_str("=> components:\n");
    let components = Components::new_with_refreshed_list();
    for component in &components {
        info.push_str(&format!("{:?}\n", component));
    }

    info
}

fn get_system_processes() -> String {
    let mut info = String::new();

    let mut sys = System::new_all();

    sys.refresh_all();

    // Add "=> processes:" to info
    info.push_str("=> processes:\n");

    // Display processes ID, name, and disk usage:
    for (pid, process) in sys.processes() {
        let path_string = match process.exe() {
            Some(path) => path.to_string_lossy().to_string(),
            None => "Unknown".to_string(),
        };
        info.push_str(&format!(
            "[{}] {:?} start_time: {:?} runtime: {} status: {} cpu_usage: {} directory: {}\n",
            pid,
            process.name(),
            process.start_time(),
            process.run_time(),
            process.status(),
            process.cpu_usage(),
            path_string,
        ));
    }

    info
}

/// returns a list of unique process names on the system.
fn get_process_names() -> Vec<String> {
    let sys = System::new_all();
    let mut process_names = Vec::new();

    for process in sys.processes().values() {
        let process_name = process.name().to_string_lossy().to_string();

        if !process_names.contains(&process_name) {
            process_names.push(process_name);
        }
    }

    process_names
}

fn kill_processes_with_name(process_name: &str) -> Option<()> {
    // Kill all processes with the given name
    let mut sys = System::new_all();
    for (pid, process) in sys.processes() {
        if process.name().to_string_lossy() == process_name {
            if process.kill() {
                debug!(
                    "Killed process with name: \"{}\" and PID: {}",
                    process_name, pid
                );
            } else {
                debug!(
                    "[Potentially] killed process with name: \"{}\" and PID: {}",
                    process_name, pid
                );
            }
        }
    }

    // sleep for a moment to give the system time to kill the processes
    std::thread::sleep(std::time::Duration::from_secs(1));

    // check if all processes with the name were killed
    sys.refresh_all();
    if get_process_names().contains(&process_name.to_string()) {
        None
    } else {
        Some(())
    }
}

fn speedtest() -> Result<String, String> {
    let output = match Command::new("speedtest-rs").output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).to_string(),
        Err(err) => {
            if err.kind() == std::io::ErrorKind::NotFound {
                return Err(
                    "speedtest-rs not found. Please install speedtest-rs and add it to your PATH. You can do this by running `cargo install speedtest-rs`".to_string(),
                );
            } else {
                return Err(format!("Failed to run speedtest-rs: {:?}", err));
            }
        }
    };

    Ok(output)
}

async fn get_location() -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct ApiResponse {
        city: Option<String>,
        regionName: Option<String>,
        country: Option<String>,
        lat: Option<f64>,
        lon: Option<f64>,
    }

    let resp = reqwest::get("http://ip-api.com/json")
        .await
        .map_err(|e| format!("Request failed: {}", e))?;
    let data: ApiResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    let city = data.city.unwrap_or_default();
    let region = data.regionName.unwrap_or_default();
    let country = data.country.unwrap_or_default();
    let lat = data.lat.unwrap_or_default();
    let lon = data.lon.unwrap_or_default();

    Ok(format!(
        "{}, {}, {} ({}, {})",
        city, region, country, lat, lon
    ))
}

fn get_currently_active_log_file() -> Option<PathBuf> {
    let mut entries: Vec<_> = fs::read_dir(&*LOGS_DIR)
        .expect("Failed to read directory")
        .filter_map(Result::ok)
        .collect();

    entries.sort_by_key(|entry| entry.file_name());

    entries.last().map(|last| last.path())
}

fn run_get_content_wait_on_file(file_path: &Path) -> Result<String, String> {
    let file_path_str = match file_path.to_str() {
        Some(path) => path,
        None => return Err("Failed to convert file path to string".to_string()),
    };
    match Command::new("cmd")
        .args([
            "/C",
            "start",
            "powershell",
            "-NoExit",
            "-Command",
            "Get-Content",
            &("\"".to_string() + file_path_str + "\""),
            "-Wait",
        ])
        .spawn()
    {
        Ok(_) => Ok("Successfully opened file in powershell".to_string()),
        Err(err) => Err(format!("Failed to open file in powershell: {:?}", err)),
    }
}

static FAILED_TEMP_FILE: LazyLock<NamedTempFile> =
    LazyLock::new(|| temp_asset!("../assets/failed.mp3"));

/// A global, lazily-initialized closure for sending paths into a channel.
static PLAY_AUDIO: LazyLock<Box<dyn Fn(&Path) + Send + Sync>> = LazyLock::new(|| {
    // Create a channel for path buffers.
    let (audio_playing_tx, audio_playing_rx) = flume::unbounded::<PathBuf>();

    // Create the audio playing thread
    // Playing audio has it's own dedicated thread because I wanted to be able to play audio
    // by passing an audio file path to a function. But the audio playing function needs to
    // have the sink and stream variable not be dropped after the end of the function.
    thread::spawn(move || {
        let sink = DefaultDeviceSink::new();

        for audio_path in audio_playing_rx.iter() {
            let file = std::fs::File::open(audio_path).unwrap();
            sink.stop();
            sink.append(rodio::Decoder::new(BufReader::new(file)).unwrap());
            // sink.play();
        }
    });

    // Return our closure, capturing the sending side of the channel.
    Box::new(move |path: &Path| {
        audio_playing_tx.send(path.to_path_buf()).unwrap();
    })
});

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _guard = set_up_logging(&LOGS_DIR);
    println!("Logs will be stored at: {}", LOGS_DIR.display());
    info!("Starting up");

    let opt = options::Opt::parse();
    let _ = dotenv();

    let ai_voice: Voice = match opt.ai_voice {
        Some(voice) => voice.into(),
        None => Voice::Echo,
    };
    let mut speak_stream = ss::SpeakStream::new(ai_voice, opt.speech_speed, opt.tick);
    if opt.mute {
        speak_stream.mute();
    }
    let speak_stream_mutex = Arc::new(Mutex::new(speak_stream));

    match opt.subcommands {
        Some(subcommand) => {
            match subcommand {
                SubCommands::ShowKeyPresses => {
                    println!("Press keys to see their codes. Press Ctrl+C to exit. Once you've figured out what key you want to use for push to talk, pass it to easy-tran using the --ptt-key argument. Or pass the number to the --special-ptt-key argument if the key is Unknown(number).");

                    fn show_keys_callback(event: Event) {
                        if let rdev::EventType::KeyPress(key) = event.event_type {
                            println!("Key pressed: {:?}", key);
                        }
                    }

                    // This will block.
                    if let Err(error) = listen(show_keys_callback) {
                        println_error(&format!("Failed to listen to key presses: {:?}", error));
                    }
                }
                SubCommands::ListDevices => {
                    let host = cpal::default_host();

                    // Set up the input device and stream with the default input config.
                    host.default_input_device();
                    let devices = host
                        .input_devices()
                        .context("Failed to get list of input devices")?;

                    for device in devices {
                        let device_name = match device.name() {
                            Ok(name) => name,
                            Err(err) => {
                                println_error(&format!("Failed to get device name: {:?}", err));
                                continue;
                            }
                        };
                        println!("{:?}", device_name);
                    }
                }
            }

            Ok(())
        }
        // Run AI
        None => {
            // Fail if ai_voice_speed out of range
            if opt.speech_speed < 0.5 || opt.speech_speed > 100.0 {
                println!("Speech speed must be between 0.5 and 100.0");
                return Ok(());
            }

            // figure out ptt key
            let ptt_key = match opt.ptt_key {
                Some(ptt_key) => ptt_key.into(),
                None => match opt.special_ptt_key {
                    Some(special_ptt_key) => rdev::Key::Unknown(special_ptt_key),
                    None => {
                        println!("No push to talk key specified. Please pass a key using the --ptt-key argument or the --special-ptt-key argument.");
                        return Ok(());
                    }
                },
            };

            if let Some(api_key) = opt.api_key {
                env::set_var("OPENAI_API_KEY", api_key);
            }

            // Fail if OPENAI_API_KEY is not set
            if env::var("OPENAI_API_KEY").is_err() {
                println!("OPENAI_API_KEY not set. Please pass your API key as an argument or assign is to the 'OPENAI_API_KEY' env var using terminal or .env file.");
                return Ok(());
            }

            let (key_handler_tx, key_handler_rx): (flume::Sender<Event>, flume::Receiver<Event>) =
                flume::unbounded();

            let (recording_tx, recording_rx): (flume::Sender<PathBuf>, flume::Receiver<PathBuf>) =
                flume::unbounded();

            let llm_should_stop_mutex = Arc::new(Mutex::new(false));

            let alarm_temp_file = temp_asset!("../assets/Dreaming of Victory.mp3");
            let (audible_timers, expired_timers_rx) =
                AudibleTimers::new(alarm_temp_file.path().to_path_buf())
                    .expect("Failed to create audible_timers");

            // Create audio recorder thread
            // This thread listens to the push to talk key and records audio when it's pressed.
            // It then sends the path of the recorded audio file to the AI thread.
            let thread_llm_should_stop_mutex = llm_should_stop_mutex.clone();
            let thread_speak_stream_mutex = speak_stream_mutex.clone();
            thread::spawn(move || {
                let mut recorder = rec::Recorder::new();
                let mut recording_start = std::time::SystemTime::now();
                let mut key_pressed = false;
                let key_to_check = ptt_key;
                let tmp_dir = tempdir().unwrap();
                let mut voice_tmp_path_option: Option<PathBuf> = None;

                for event in key_handler_rx.iter() {
                    match event.event_type {
                        rdev::EventType::KeyPress(key) => {
                            if key == key_to_check && !key_pressed {
                                key_pressed = true;
                                // handle key press

                                audible_timers.stop_alarm();

                                // stop the AI voice from speaking
                                {
                                    // stop the LLM
                                    let mut llm_should_stop =
                                        thread_llm_should_stop_mutex.lock().unwrap();
                                    *llm_should_stop = true;
                                    drop(llm_should_stop);

                                    let mut thread_speak_stream =
                                        thread_speak_stream_mutex.lock().unwrap();
                                    thread_speak_stream.stop_speech();
                                    drop(thread_speak_stream);
                                }

                                let random_filename = format!("{}.wav", Uuid::new_v4());
                                let voice_tmp_path = tmp_dir.path().join(random_filename);
                                voice_tmp_path_option = Some(voice_tmp_path.clone());

                                recording_start = std::time::SystemTime::now();
                                match recorder.start_recording(&voice_tmp_path, Some(&opt.device)) {
                                    Ok(_) => info!("Recording started"),
                                    Err(err) => println_error(&format!(
                                        "Failed to start recording: {:?}",
                                        err
                                    )),
                                }
                            }
                        }
                        rdev::EventType::KeyRelease(key) => {
                            if key == key_to_check && key_pressed {
                                key_pressed = false;
                                // handle key release

                                // stop any alarms
                                audible_timers.stop_alarm();

                                // get elapsed time since recording started
                                let elapsed_option = match recording_start.elapsed() {
                                    Ok(elapsed) => Some(elapsed),
                                    Err(err) => {
                                        println_error(&format!(
                                            "Failed to get elapsed recording time: {:?}",
                                            err
                                        ));
                                        None
                                    }
                                };

                                // stop recording
                                match recorder.stop_recording() {
                                    Ok(_) => info!("Recording stopped"),
                                    Err(err) => {
                                        println_error(&format!(
                                            "Failed to stop recording: {:?}",
                                            err
                                        ));
                                        continue;
                                    }
                                }

                                // continue if we failed to get elapsed time
                                let elapsed = match elapsed_option {
                                    Some(elapsed) => elapsed,
                                    None => {
                                        continue;
                                    }
                                };

                                // Whisper API can't handle less than 0.1 seconds of audio.
                                // So we'll only transcribe if the recording is longer than 0.2 seconds.
                                if elapsed.as_secs_f32() < 0.2 {
                                    println_error("User recording too short. Aborting transctiption and LLM response.");
                                    continue;
                                };

                                if let Some(voice_tmp_path) = voice_tmp_path_option.take() {
                                    recording_tx.send(voice_tmp_path.clone()).unwrap();
                                }
                            }
                        }
                        _ => (),
                    }
                }
            });

            let (llm_messages_tx, llm_messages_rx): (
                flume::Sender<Message>,
                flume::Receiver<Message>,
            ) = flume::unbounded();

            // Create timer to llm message thread
            // This thread listens to the expired timers channel and sends a message to the AI thread
            // when a timer expires.
            let thread_llm_messages_tx = llm_messages_tx.clone();
            thread::spawn(move || {
                for timer in expired_timers_rx.iter() {
                    let timer_string = &format!(
                        "Timer_ID: \"{}\" Timer_description: \"{}\" goes off at time: \"{}\"",
                        timer.id,
                        timer.description,
                        timer.timestamp.to_rfc3339(),
                    );
                    thread_llm_messages_tx.send(
                        Message::Function { fn_name: "check_on_timers".to_string(), content: format!("A timer has gone off with the following details. Depending on what the timer is for, alert the user a timer at the given time has gone off, and tell them what it's time for them to do. Or take independent action accordingly.\n{}", timer_string)}
                    ).unwrap();
                }
            });

            // Create user audio to text thread
            // This thread listens to the audio recorder thread and transcribes the audio
            // before feeding it to the AI assistant.
            let thread_speak_stream_mutex = speak_stream_mutex.clone();
            let thread_llm_messages_tx = llm_messages_tx.clone();
            thread::spawn(move || {
                let client = Client::new();

                let runtime = tokio::runtime::Runtime::new()
                    .context("Failed to create tokio runtime")
                    .unwrap();

                for audio_path in recording_rx.iter() {
                    let mut thread_speak_stream = thread_speak_stream_mutex.lock().unwrap();
                    thread_speak_stream.stop_speech();
                    drop(thread_speak_stream);

                    info!("Transcribing user audio");
                    let transcription_result = match runtime.block_on(future::timeout(
                        Duration::from_secs(10),
                        transcribe::transcribe(&client, &audio_path),
                    )) {
                        Ok(transcription_result) => transcription_result,
                        Err(err) => {
                            println_error(&format!(
                                "Failed to transcribe audio due to timeout: {:?}",
                                err
                            ));

                            PLAY_AUDIO(FAILED_TEMP_FILE.path());

                            continue;
                        }
                    };

                    let transcription = match transcription_result {
                        Ok(transcription) => transcription,
                        Err(err) => {
                            println_error(&format!("Failed to transcribe audio: {:?}", err));
                            PLAY_AUDIO(FAILED_TEMP_FILE.path());
                            continue;
                        }
                    };

                    if transcription.is_empty() {
                        println!("No transcription");
                        info!("User transcription was empty. Aborting LLM response.");
                        continue;
                    }

                    thread_llm_messages_tx
                        .send(Message::User {
                            content: transcription,
                        })
                        .unwrap();
                }
            });

            // Create AI thread
            // This thread receives new llm messages and processes them with the AI.
            let thread_llm_should_stop_mutex = llm_should_stop_mutex.clone();
            let thread_speak_stream_mutex = speak_stream_mutex.clone();
            thread::spawn(move || {
                let client = Client::new();
                let mut message_history: Vec<ChatCompletionRequestMessage> = Vec::new();

                message_history.push(
                    ChatCompletionRequestSystemMessageArgs::default()
                        .content("You are a desktop voice assistant. The messages you receive from the user are voice transcriptions. Your responses will be spoken out loud by a text to speech engine. You should be helpful but concise. As conversations should be a back and forth. Don't make audio clips that run on for more than 15 seconds. Also don't ask 'if I would like to know more'. If you are told to set a timer, you should always call the \"set_timer_at\" function.")
                        .build()
                        .unwrap()
                        .into(),
                );

                let runtime = tokio::runtime::Runtime::new()
                    .context("Failed to create tokio runtime")
                    .unwrap();

                for llm_message in llm_messages_rx.iter() {
                    // convert message type to ChatCompletionRequestMessage
                    match llm_message {
                        Message::System { content } => {
                            message_history.push(
                                ChatCompletionRequestSystemMessageArgs::default()
                                    .content(content)
                                    .build()
                                    .unwrap()
                                    .into(),
                            );
                        }
                        Message::User { content } => {
                            // Add time header to user message
                            let time_header = format!("Local Time: {}", Local::now());
                            let user_message = time_header + "\n" + &content;

                            message_history.push(
                                ChatCompletionRequestUserMessageArgs::default()
                                    .content(user_message)
                                    .build()
                                    .unwrap()
                                    .into(),
                            );

                            println!("{}", "You: ".truecolor(0, 255, 0));
                            println!("{}", content);
                            info!("User transcription: \"{}\"", truncate(&content, 20));
                        }
                        Message::Assistant { content } => {
                            message_history.push(
                                ChatCompletionRequestAssistantMessageArgs::default()
                                    .content(content)
                                    .build()
                                    .unwrap()
                                    .into(),
                            );
                        }
                        Message::Tool { content } => {
                            message_history.push(
                                ChatCompletionRequestToolMessageArgs::default()
                                    .content(content)
                                    .build()
                                    .unwrap()
                                    .into(),
                            );
                        }
                        Message::Function { content, fn_name } => {
                            message_history.push(
                                ChatCompletionRequestFunctionMessageArgs::default()
                                    .content(content)
                                    .name(fn_name)
                                    .build()
                                    .unwrap()
                                    .into(),
                            );
                        }
                    };

                    // Make sure the LLM token generation is allowed to start
                    // It should only be stopped when the LLM is running.
                    // Since it's not running now, it should be allowed to start.
                    let mut llm_should_stop = thread_llm_should_stop_mutex.lock().unwrap();
                    *llm_should_stop = false;
                    drop(llm_should_stop);

                    // repeatedly create request until it's answered
                    let mut displayed_ai_label = false;
                    'request: loop {
                        debug!("Entered chat completion request loop");
                        let mut ai_content = String::new();
                        let request = CreateChatCompletionRequestArgs::default()
                            // .model("gpt-3.5-turbo")
                            .model(&opt.model)
                            .max_tokens(512u16)
                            .messages(message_history.clone())
                            .functions([
                                ChatCompletionFunctionsArgs::default()
                                .description("Sets the brightness of the device's screen.")
                                .name("set_screen_brightness")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {
                                            "brightness": {
                                                "type": "string",
                                                "description": "The brightness of the screen. A number between 0 and 100.",
                                            },
                                        },
                                        "required": ["brightness"],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("media_controls")
                                    .description("Plays/Pauses/Seeks media.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {
                                            "media_button": { "type": "string", "enum": ["MediaStop", "MediaNextTrack", "MediaPlayPause", "MediaPrevTrack", "VolumeUp", "VolumeDown", "VolumeMute"] },
                                        },
                                        "required": ["media_button"],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("set_volume")
                                    .description("Sets the system volume on Windows to a value between 0 and 100.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": { "volume": { "type": "integer" } },
                                        "required": ["volume"],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("open_application")
                                    .description("naively opens an application by pressing the super key to open system search and then types the name of the application and presses enter.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {
                                            "application": { "type": "string" },
                                        },
                                        "required": ["application"],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("open_logs_folder")
                                    .description("Opens this program's logging folder in the default file browser for the user to see.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {},
                                        "required": [],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("open_openai_billing")
                                    .description("Opens the OpenAI usage dashboard in the default web browser.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {},
                                        "required": [],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("open_assistance_repo")
                                    .description("Opens the AI Assistance repository in the default web browser.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {},
                                        "required": [],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("sysinfo")
                                    .description("Returns this system's information.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {},
                                        "required": [],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("get_system_processes")
                                    .description("Returns this system's processes with their pid, name, start_time, runtime, status, cpu_usage, exe_path and other information.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {},
                                        "required": [],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("kill_processes_with_name")
                                    .description("Kills all processes with a given name. ALWAYS call \"get_system_processes\" first to get the name of the process you want to kill.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {
                                            "process_name": { "type": "string" },
                                        },
                                        "required": ["process_name"],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("speedtest")
                                    .description("Runs an internet speedtest and returns the results.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {},
                                        "required": [],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("set_timer_at")
                                    .description("Sets a timer to go off at a specific time. Pass the time as rfc3339 datetime string. Example: \"2024-12-04T00:44:00-08:00\". The description field is optional, add descriptions that will tell you what to remind the user to do, if anything, after the timer goes off.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {
                                            "time": { "type": "string" },
                                            "description": { "type": "string" },
                                        },
                                        "required": ["time"],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("check_on_timers")
                                    .description("Displays all timers that are currently set. The time they are set to go off and the duration remaining until they go off.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {},
                                        "required": [],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("delete_timer_by_id")
                                    .description("Deletes a timer by it's ID. Pass the ID of the timer you want to delete. To get the ID of a timer, call the \"check_on_timers\" function.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {
                                            "timer_id": { "type": "integer" },
                                        },
                                        "required": ["timer_id"],
                                    }))
                                    .build().unwrap(),

                                    ChatCompletionFunctionsArgs::default()
                                    .name("show_live_log_stream")
                                    .description("Shows live updates of the log file via opening powershell and running 'Get-Content -Path \"path/to/log/file\" -Wait'.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {},
                                        "required": [],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("set_clipboard")
                                    .description("Sets the clipboard to the given text.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {
                                            "clipboard_text": { "type": "string" },
                                        },
                                        "required": ["clipboard_text"],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("set_speech_speed")
                                    .description("Sets how fast the AI voice speaks. Speed must be between 0.5 and 100.0.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": { "speed": { "type": "number" } },
                                        "required": ["speed"],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("get_speech_speed")
                                    .description("Returns the current AI voice speech speed.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {},
                                        "required": [],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("mute_speech")
                                    .description("Mutes the AI voice so no speech is played.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {},
                                        "required": [],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("unmute_speech")
                                    .description("Unmutes the AI voice so responses are spoken again.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {},
                                        "required": [],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("set_voice")
                                    .description("Changes the AI speaking voice. Pass one of: alloy, echo, fable, onyx, nova, shimmer.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": { "voice": { "type": "string" } },
                                        "required": ["voice"],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("get_voice")
                                    .description("Returns the name of the current AI voice.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {},
                                        "required": [],
                                    }))
                                    .build().unwrap(),

                              ChatCompletionFunctionsArgs::default()
                                    .name("get_location")
                                    .description("Returns an approximate location based on the machine's IP address.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {},
                                        "required": [],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("list_output_devices")
                                    .description("Lists available audio output devices. The default device is marked with '(Default)'.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {},
                                        "required": [],
                                    }))
                                    .build().unwrap(),

                                ChatCompletionFunctionsArgs::default()
                                    .name("set_output_device")
                                    .description("Sets the audio output device by name. Pass 'Default Audio Output Device' to use the system default.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": { "device_name": { "type": "string" } },
                                        "required": ["device_name"],
                                    }))
                                    .build().unwrap(),
                            ])
                            .build()
                            .unwrap();

                        let mut stream = match runtime.block_on(future::timeout(
                            Duration::from_secs(15),
                            client.chat().create_stream(request),
                        )) {
                            Ok(stream) => match stream {
                                Ok(stream) => stream,
                                Err(err) => {
                                    println_error(&format!("Failed to create stream: {}", err));

                                    PLAY_AUDIO(FAILED_TEMP_FILE.path());

                                    break 'request;
                                }
                            },
                            Err(err) => {
                                println_error(&format!(
                                    "Failed to create stream due to timeout: {:?}",
                                    err
                                ));

                                PLAY_AUDIO(FAILED_TEMP_FILE.path());

                                break 'request;
                            }
                        };

                        let mut fn_name = String::new();
                        let mut fn_args = String::new();
                        let mut inside_code_block = false;
                        let mut last_codeblock_line_option: Option<usize> = None;
                        let mut figure_number = 1;

                        debug!("Starting AI response token generation.");
                        while let Some(result) = {
                            match runtime
                                .block_on(future::timeout(Duration::from_secs(15), stream.next()))
                            {
                                Ok(result) => result,
                                Err(err) => {
                                    println_error(&format!(
                                        "Failed to get response from AI due to timeout: {:?}",
                                        err
                                    ));

                                    PLAY_AUDIO(FAILED_TEMP_FILE.path());

                                    break 'request;
                                }
                            }
                        } {
                            let mut llm_should_stop = thread_llm_should_stop_mutex.lock().unwrap();

                            if *llm_should_stop {
                                *llm_should_stop = false;

                                info!("AI response token generation manually stopped.");

                                // remember what the AI said so far.
                                message_history.push(
                                    ChatCompletionRequestAssistantMessageArgs::default()
                                        .content(ai_content)
                                        .build()
                                        .unwrap()
                                        .into(),
                                );

                                println!();

                                break 'request;
                            }
                            drop(llm_should_stop);

                            match result {
                                Ok(response) => {
                                    for chat_choice in response.choices {
                                        #[allow(deprecated)]
                                        if let Some(fn_call) = &chat_choice.delta.function_call {
                                            if let Some(name) = &fn_call.name {
                                                fn_name = name.clone();
                                            }
                                            if let Some(args) = &fn_call.arguments {
                                                fn_args.push_str(args);
                                            }
                                        }
                                        if let Some(finish_reason) = &chat_choice.finish_reason {
                                            if matches!(finish_reason, FinishReason::FunctionCall) {
                                                let func_response_option = call_fn(
                                                    &fn_name,
                                                    &fn_args,
                                                    llm_messages_tx.clone(),
                                                    &thread_speak_stream_mutex,
                                                );

                                                if let Some(func_response) = func_response_option {
                                                    message_history.push(
                                                        ChatCompletionRequestFunctionMessageArgs::default()
                                                            .name(fn_name.clone())
                                                            .content(func_response)
                                                            .build()
                                                            .unwrap()
                                                            .into(),
                                                    );

                                                    continue 'request;
                                                }
                                            }
                                        } else if let Some(content) = &chat_choice.delta.content {
                                            if !displayed_ai_label {
                                                println!("{}", "AI: ".truecolor(0, 0, 255));
                                                displayed_ai_label = true;
                                            }

                                            print!("{}", content);
                                            ai_content += content;

                                            let mut last_non_empty_line_option = None;
                                            // return the last non empty line and its line number
                                            for (line_num, line_content) in
                                                ai_content.lines().enumerate()
                                            {
                                                if !line_content.is_empty() {
                                                    last_non_empty_line_option =
                                                        Some((line_num, line_content));
                                                }
                                            }

                                            fn mark_inside_code_block(
                                                inside_code_block: &mut bool,
                                                last_codeblock_line_option: &mut Option<usize>,
                                                line_num: usize,
                                                figure_number: &mut i32,
                                                thread_speak_stream_mutex: &Arc<Mutex<SpeakStream>>,
                                            ) {
                                                *inside_code_block = true;
                                                *last_codeblock_line_option = Some(line_num);

                                                // add figure text
                                                let see_figure_message =
                                                    format!(". See figure {}... ", figure_number);
                                                *figure_number += 1;
                                                // speak figure message
                                                let mut thread_speak_stream =
                                                    thread_speak_stream_mutex.lock().unwrap();
                                                thread_speak_stream.add_token(&see_figure_message);
                                                drop(thread_speak_stream);

                                                // Tells the ai voice to speak the remaining text in the buffer
                                                let mut thread_speak_stream =
                                                    thread_speak_stream_mutex.lock().unwrap();
                                                thread_speak_stream.complete_sentence();
                                                drop(thread_speak_stream);
                                            }

                                            if let Some((line_num, line_content)) =
                                                last_non_empty_line_option
                                            {
                                                match last_codeblock_line_option {
                                                    Some(last_codeblock_line) => {
                                                        if last_codeblock_line != line_num {
                                                            match inside_code_block {
                                                                false => {
                                                                    if line_content
                                                                        .starts_with("```")
                                                                    {
                                                                        mark_inside_code_block(&mut inside_code_block, &mut last_codeblock_line_option, line_num, &mut figure_number, &thread_speak_stream_mutex);
                                                                    }
                                                                }
                                                                true => {
                                                                    if line_content.ends_with("```")
                                                                    {
                                                                        inside_code_block = false;

                                                                        last_codeblock_line_option = Some(line_num);
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                    None => {
                                                        if line_content.starts_with("```") {
                                                            mark_inside_code_block(
                                                                &mut inside_code_block,
                                                                &mut last_codeblock_line_option,
                                                                line_num,
                                                                &mut figure_number,
                                                                &thread_speak_stream_mutex,
                                                            );
                                                        }
                                                    }
                                                }
                                            }

                                            if !inside_code_block {
                                                let mut thread_speak_stream =
                                                    thread_speak_stream_mutex.lock().unwrap();
                                                thread_speak_stream.add_token(content);
                                                drop(thread_speak_stream);
                                            }
                                        }
                                    }
                                }
                                Err(_err) => {
                                    println!("error: {_err}");
                                    warn!("OpenAI API response error: {:?}", _err);
                                    if message_history.len() > 1 {
                                        // remove 1 instead of 0 because the first message is a system message
                                        message_history.remove(1);

                                        println!(
                                            "Removed message from message history. There are now {} remembered messages",
                                            message_history.len()
                                        );
                                        debug!(
                                            "Removed message from message history. There are now {} remembered messages",
                                            message_history.len()
                                        );
                                    }
                                    continue 'request;
                                }
                            }
                            stdout().flush().unwrap();
                        }
                        println!();

                        message_history.push(
                            ChatCompletionRequestAssistantMessageArgs::default()
                                .content(ai_content)
                                .build()
                                .unwrap()
                                .into(),
                        );
                        break;
                    }

                    // Tells the ai voice to speak the remaining text in the buffer
                    let mut thread_speak_stream = thread_speak_stream_mutex.lock().unwrap();
                    thread_speak_stream.complete_sentence();
                    drop(thread_speak_stream);
                    debug!("AI token generation complete.");
                }
            });

            info!("System ready");

            // Have this main thread receive events and send them to the key handler thread
            {
                // We send keys as a message in a channel instead of putting the key handler
                // inside the callback function because the operating system's mouse and
                // inputs freeze up when the callback is happening.
                let callback = move |event: Event| {
                    key_handler_tx.send(event).unwrap();
                };

                // This will block.
                if let Err(error) = listen(callback) {
                    println_error(&format!("Failed to listen to key presses: {:?}", error));
                }
            }

            Ok(())
        }
    }
}
