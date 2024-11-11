use anyhow::Context;
use async_openai::types::{
    ChatCompletionFunctionsArgs, ChatCompletionRequestFunctionMessageArgs, FinishReason,
};
use dotenvy::dotenv;
use serde_json::{de, json};
use std::env;
use std::fs::File;
use std::io::{stdout, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use tempfile::{tempdir, NamedTempFile};
use tracing_subscriber::filter::FilterFn;
use tracing_subscriber::Registry;
mod transcribe;
use chrono::Local;
use futures::stream::StreamExt; // For `.next()` on FuturesOrdered.
use std::thread;
use tempfile::Builder;
mod record;
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
use std::time::Duration;
use uuid::Uuid;
mod easy_rdev_key;
mod speakstream;
use enigo::{Enigo, KeyboardControllable};
use speakstream::ss;
mod options;
use tracing::{debug, error, info, trace, warn};
use tracing_appender::rolling::{RollingFileAppender, Rotation};

use crate::speakstream::ss::SpeakStream;

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

fn error_and_panic(s: &str) -> ! {
    error!("A fatal error occured: {}", s);
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

fn println_error(err: &str) {
    println!("{}: {}", "Error".truecolor(255, 0, 0), err);
    warn!("{}", err);
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let logs_dir = dirs::cache_dir()
        .unwrap()
        .join("quick-assistant")
        .join("logs");
    println!("Logs will be stored at: {}", logs_dir.display());
    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_suffix("quick-assistant.log")
        .build(logs_dir)
        .expect("failed to initialize rolling file appender");

    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

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

    info!("Starting up");

    let mut enigo = Enigo::new();

    let opt = options::Opt::parse();
    let _ = dotenv();

    let ai_voice: Voice = match opt.ai_voice {
        Some(voice) => voice.into(),
        None => Voice::Echo,
    };
    let (speak_stream, _stream) = ss::SpeakStream::new(ai_voice, opt.speech_speed);
    let speak_stream_mutex = Arc::new(Mutex::new(speak_stream));

    let (audio_playing_tx, audio_playing_rx): (flume::Sender<PathBuf>, flume::Receiver<PathBuf>) =
        flume::unbounded();

    let play_audio = move |path: &Path| {
        audio_playing_tx.send(path.to_path_buf()).unwrap();
    };

    let failed_temp_file =
        create_temp_file_from_bytes(include_bytes!("../assets/failed.mp3"), ".mp3");

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

                                // get elapsed time since recording started
                                let elapsed = match recording_start.elapsed() {
                                    Ok(elapsed) => elapsed,
                                    Err(err) => {
                                        println_error(&format!(
                                            "Failed to get elapsed recording time: {:?}",
                                            err
                                        ));
                                        let _ = recorder.stop_recording();
                                        info!("Recording stopped");
                                        continue;
                                    }
                                };
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

            // Create AI thread
            // This thread listens to the audio recorder thread and transcribes the audio
            // before feeding it to the AI assistant.
            let thread_llm_should_stop_mutex = llm_should_stop_mutex.clone();
            let thread_speak_stream_mutex = speak_stream_mutex.clone();
            thread::spawn(move || {
                let client = Client::new();
                let mut message_history: Vec<ChatCompletionRequestMessage> = Vec::new();

                message_history.push(
                    ChatCompletionRequestSystemMessageArgs::default()
                        .content("You are a desktop voice assistant. The messages you receive from the user are voice transcriptions. Your responses will be spoken out loud by a text to speech engine. You should be helpful but concise. As conversations should be a back and forth. Don't make audio clips that run on for more than 15 seconds. Also don't ask 'if I would like to know more'")
                        .build()
                        .unwrap()
                        .into(),
                );

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

                            play_audio(failed_temp_file.path());

                            continue;
                        }
                    };

                    let mut transcription = match transcription_result {
                        Ok(transcription) => transcription,
                        Err(err) => {
                            println_error(&format!("Failed to transcribe audio: {:?}", err));
                            continue;
                        }
                    };

                    if let Some(last_char) = transcription.chars().last() {
                        if ['.', '?', '!', ','].contains(&last_char) {
                            transcription.push(' ');
                        }
                    }

                    if transcription.is_empty() {
                        println!("No transcription");
                        info!("User transcription was empty. Aborting LLM response.");
                        continue;
                    }

                    println!("{}", "You: ".truecolor(0, 255, 0));
                    println!("{}", transcription);
                    info!("User transcription: \"{}\"", truncate(&transcription, 20));

                    let time_header = format!("Local Time: {}", Local::now());
                    let user_message = time_header + "\n" + &transcription;

                    message_history.push(
                        ChatCompletionRequestUserMessageArgs::default()
                            .content(user_message)
                            .build()
                            .unwrap()
                            .into(),
                    );

                    // Make sure the LLM token generation is allowed to start
                    // It should only be stopped when the LLM is running.
                    // Since it's not running now, it should be allowed to start.
                    let mut llm_should_stop = thread_llm_should_stop_mutex.lock().unwrap();
                    *llm_should_stop = false;
                    drop(llm_should_stop);

                    // repeatedly create request until it's answered
                    let mut displayed_ai_label = false;
                    'request: loop {
                        let mut ai_content = String::new();
                        let request = CreateChatCompletionRequestArgs::default()
                            // .model("gpt-3.5-turbo")
                            .model(&opt.model)
                            .max_tokens(512u16)
                            .messages(message_history.clone())
                            .functions([
                                ChatCompletionFunctionsArgs::default()
                                    .name("set_screen_brightness")
                                    .description("Sets the brightness of the device's screen.")
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
                                    .name("open_application")
                                    .description("naively opens an applicatin by pressing the super key to open system search and then types the name of the application and presses enter.")
                                    .parameters(json!({
                                        "type": "object",
                                        "properties": {
                                            "application": { "type": "string" },
                                        },
                                        "required": ["application"],
                                    }))
                                    .build().unwrap()
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

                                    play_audio(failed_temp_file.path());

                                    break 'request;
                                }
                            },
                            Err(err) => {
                                println_error(&format!(
                                    "Failed to create stream due to timeout: {:?}",
                                    err
                                ));

                                play_audio(failed_temp_file.path());

                                break 'request;
                            }
                        };

                        let mut fn_name = String::new();
                        let mut fn_args = String::new();
                        let mut inside_code_block = false;
                        // negative number to indicate that the last codeblock line is unknown
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

                                    play_audio(failed_temp_file.path());

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
                                        // .role(Role::Assistant)
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
                                                // call_fn(&client, &fn_name, &fn_args).await?;

                                                let func_response = match fn_name.as_str() {
                                                    "set_screen_brightness" => {
                                                        info!("Handling set_screen_brightness function call.");
                                                        let args: serde_json::Value =
                                                            serde_json::from_str(&fn_args).unwrap();
                                                        let brightness = args["brightness"]
                                                            .as_str()
                                                            .unwrap()
                                                            .parse::<u32>()
                                                            .unwrap();

                                                        println!(
                                                            "{}{}",
                                                            "set_screen_brightness: ".purple(),
                                                            brightness
                                                        );

                                                        if set_screen_brightness(brightness)
                                                            .is_some()
                                                        {
                                                            Some("Brightness set")
                                                        } else {
                                                            Some("Failed to set brightness")
                                                        }
                                                    }
                                                    "media_controls" => {
                                                        info!("Handling media_controls function call.");
                                                        let args: serde_json::Value =
                                                            serde_json::from_str(&fn_args).unwrap();
                                                        let media_button =
                                                            args["media_button"].as_str().unwrap();

                                                        println!(
                                                            "{}{}",
                                                            "media_controls: ".purple(),
                                                            media_button
                                                        );

                                                        match media_button {
                                                            "MediaStop" => {
                                                                enigo.key_click(
                                                                    enigo::Key::MediaStop,
                                                                );
                                                                info!("MediaStop");
                                                            }
                                                            "MediaNextTrack" => {
                                                                enigo.key_click(
                                                                    enigo::Key::MediaNextTrack,
                                                                );
                                                                info!("MediaNextTrack");
                                                            }
                                                            "MediaPlayPause" => {
                                                                enigo.key_click(
                                                                    enigo::Key::MediaPlayPause,
                                                                );
                                                                info!("MediaPlayPause");
                                                            }
                                                            "MediaPrevTrack" => {
                                                                enigo.key_click(
                                                                    enigo::Key::MediaPrevTrack,
                                                                );
                                                                enigo.key_click(
                                                                    enigo::Key::MediaPrevTrack,
                                                                );
                                                                info!("MediaPrevTrack");
                                                            }
                                                            "VolumeUp" => {
                                                                for _ in 0..5 {
                                                                    enigo.key_click(
                                                                        enigo::Key::VolumeUp,
                                                                    );
                                                                }
                                                                info!("VolumeUp");
                                                            }
                                                            "VolumeDown" => {
                                                                for _ in 0..5 {
                                                                    enigo.key_click(
                                                                        enigo::Key::VolumeDown,
                                                                    );
                                                                }
                                                                info!("VolumeDown");
                                                            }
                                                            "VolumeMute" => {
                                                                enigo.key_click(
                                                                    enigo::Key::VolumeMute,
                                                                );
                                                                info!("VolumeMute");
                                                            }
                                                            _ => {
                                                                println!(
                                                                    "Unknown media button: {}",
                                                                    media_button
                                                                );
                                                                warn!(
                                                                    "AI called unknown media button: {}",
                                                                    media_button
                                                                );
                                                            }
                                                        }

                                                        None
                                                    }

                                                    "open_application" => {
                                                        info!("Handling open_application function call.");
                                                        let args: serde_json::Value =
                                                            serde_json::from_str(&fn_args).unwrap();
                                                        let application =
                                                            args["application"].as_str().unwrap();

                                                        println!(
                                                            "{}{}",
                                                            "opening application: ".purple(),
                                                            application
                                                        );

                                                        enigo.key_click(enigo::Key::Meta);
                                                        std::thread::sleep(
                                                            std::time::Duration::from_millis(500),
                                                        );
                                                        enigo.key_sequence(application);
                                                        std::thread::sleep(
                                                            std::time::Duration::from_millis(500),
                                                        );
                                                        enigo.key_click(enigo::Key::Return);

                                                        None
                                                    }
                                                    _ => {
                                                        println!("Unknown function: {}", fn_name);
                                                        warn!(
                                                            "AI called unknown function: {}",
                                                            fn_name
                                                        );

                                                        None
                                                    }
                                                };

                                                // println!("func_response: \"{:?}\"", func_response);

                                                if let Some(func_response) = func_response {
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
                                            // return the last non empy line and it's line number
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
                                                // print!("{}", "inside_code_block = true;".truecolor(255, 0, 255));
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
                                                                    // println!("{}", "inside_code_block is true".truecolor(0, 0, 255));
                                                                    if line_content.ends_with("```")
                                                                    {
                                                                        inside_code_block = false;

                                                                        // print!("{}", "inside_code_block = false;".truecolor(255, 0, 255));
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
                                    if !message_history.len() > 1 {
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

            // Create the audio playing thread
            // Playing audio has it's own dedicated thread because I wanted to be able to play audio
            // by passing an audio file path to a function. But the audio playing function needs to
            // have the sink and stream variable not be dropped after the end of the function.
            tokio::spawn(async move {
                let (_stream, stream_handle) = rodio::OutputStream::try_default().unwrap();
                let sink = rodio::Sink::try_new(&stream_handle).unwrap();

                for audio_path in audio_playing_rx.iter() {
                    let file = std::fs::File::open(audio_path).unwrap();
                    sink.stop();
                    sink.append(rodio::Decoder::new(BufReader::new(file)).unwrap());
                    // sink.play();
                }
            });

            // Have this main thread recieve events and send them to the key handler thread
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
