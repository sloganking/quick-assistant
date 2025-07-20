use async_openai::{
    config::OpenAIConfig,
    types::{
        AssistantStreamEvent, CreateAssistantRequestArgs, CreateMessageRequest, CreateRunRequest,
        CreateThreadRequest, FunctionObject, MessageDeltaContent, MessageRole, RunObject,
        SubmitToolOutputsRunRequest, ToolsOutputs, Voice,
    },
    Client,
};
use clap::Parser;
use clipboard::{ClipboardContext, ClipboardProvider};
use colored::Colorize;
use dotenvy::dotenv;
use futures::StreamExt;
use open;
use enigo::{Enigo, KeyboardControllable};
use speakstream::ss::SpeakStream;
use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod record;
mod transcribe;
use flume::Sender;
use rdev::{listen, Event, EventType, Key};
use record::rec;
use std::thread;
use tempfile::tempdir;
use uuid::Uuid;
use sysinfo::{Components, Disks, Networks, System};

#[derive(Parser, Debug)]
struct Opt {
    /// How fast the AI speaks. 1.0 is normal speed.
    #[arg(long, default_value_t = 1.0)]
    speech_speed: f32,

    /// Enable ticking sound while speaking.
    #[arg(long, default_value_t = false)]
    tick: bool,

    /// Enable audio ducking while the assistant is speaking.
    #[arg(long, default_value_t = false)]
    duck: bool,

    /// Enable audio ducking while the push-to-talk key is held.
    #[arg(long, default_value_t = false)]
    duck_ptt: bool,
}

fn parse_voice(name: &str) -> Option<Voice> {
    match name.to_lowercase().as_str() {
        "alloy" => Some(Voice::Alloy),
        "ash" => Some(Voice::Ash),
        "coral" => Some(Voice::Coral),
        "echo" => Some(Voice::Echo),
        "fable" => Some(Voice::Fable),
        "onyx" => Some(Voice::Onyx),
        "nova" => Some(Voice::Nova),
        "sage" => Some(Voice::Sage),
        "shimmer" => Some(Voice::Shimmer),
        _ => None,
    }
}

fn voice_to_str(voice: &Voice) -> &'static str {
    match voice {
        Voice::Alloy => "alloy",
        Voice::Ash => "ash",
        Voice::Coral => "coral",
        Voice::Echo => "echo",
        Voice::Fable => "fable",
        Voice::Onyx => "onyx",
        Voice::Nova => "nova",
        Voice::Sage => "sage",
        Voice::Shimmer => "shimmer",
        _ => "unknown",
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let opt = Opt::parse();
    let _ = dotenv();

    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    let client = Client::new();

    let create_assistant_request = CreateAssistantRequestArgs::default()
        .instructions("You are a weather bot. Use the provided functions to answer questions.")
        .model("gpt-4o")
        .tools(vec![
            FunctionObject {
                name: "get_current_temperature".into(),
                description: Some(
                    "Get the current temperature for a specific location".into(),
                ),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "location": {
                            "type": "string",
                            "description": "The city and state, e.g., San Francisco, CA"
                        },
                        "unit": {
                            "type": "string",
                            "enum": ["Celsius", "Fahrenheit"],
                            "description": "The temperature unit to use. Infer this from the user's location.",
                        }
                    },
                    "required": ["location", "unit"]
                })),
                strict: None,
            }
            .into(),
            FunctionObject {
                name: "get_rain_probability".into(),
                description: Some(
                    "Get the probability of rain for a specific location".into(),
                ),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "location": {
                            "type": "string",
                            "description": "The city and state, e.g., San Francisco, CA"
                        }
                    },
                    "required": ["location"]
                })),
                strict: None,
            }
            .into(),
            FunctionObject {
                name: "speedtest".into(),
                description: Some(
                    "Runs an internet speedtest and returns the results.".into(),
                ),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                })),
                strict: None,
            }
            .into(),
            FunctionObject {
                name: "set_screen_brightness".into(),
                description: Some(
                    "Sets the screen brightness from 0 to 100 using the `luster` utility.".into(),
                ),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"brightness": {"type": "integer"}},
                    "required": ["brightness"],
                })),
                strict: None,
            }
            .into(),
            FunctionObject {
                name: "media_controls".into(),
                description: Some("Plays, pauses or seeks media.".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "media_button": {
                            "type": "string",
                            "enum": [
                                "MediaStop",
                                "MediaNextTrack",
                                "MediaPlayPause",
                                "MediaPrevTrack",
                                "VolumeUp",
                                "VolumeDown",
                                "VolumeMute"
                            ]
                        }
                    },
                    "required": ["media_button"]
                })),
                strict: None,
            }
            .into(),
            FunctionObject {
                name: "set_clipboard".into(),
                description: Some("Sets the clipboard to the given text.".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"clipboard_text": {"type": "string"}},
                    "required": ["clipboard_text"]
                })),
                strict: None,
            }
            .into(),
            FunctionObject {
                name: "get_clipboard".into(),
                description: Some("Returns the current clipboard text.".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                })),
                strict: None,
            }
            .into(),
            FunctionObject {
                name: "open_openai_billing".into(),
                description: Some(
                    "Opens the OpenAI usage dashboard in the default web browser.".into(),
                ),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                })),
                strict: None,
            }
            .into(),
            FunctionObject {
                name: "get_system_info".into(),
                description: Some("Returns system information like CPU and memory usage.".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                })),
                strict: None,
            }
            .into(),
            FunctionObject {
                name: "set_speech_speed".into(),
                description: Some(
                    "Sets how fast the AI voice speaks. Speed must be between 0.5 and 100.".into(),
                ),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"speed": {"type": "number"}},
                    "required": ["speed"],
                })),
                strict: None,
            }
            .into(),
            FunctionObject {
                name: "get_speech_speed".into(),
                description: Some("Returns the current AI voice speech speed.".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                })),
                strict: None,
            }
            .into(),
            FunctionObject {
                name: "mute_speech".into(),
                description: Some("Mutes the AI voice output.".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                })),
                strict: None,
            }
            .into(),
            FunctionObject {
                name: "unmute_speech".into(),
                description: Some("Unmutes the AI voice output.".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                })),
                strict: None,
            }
            .into(),
            FunctionObject {
                name: "set_voice".into(),
                description: Some(
                    "Changes the AI speaking voice. Pass one of: alloy, ash, coral, echo, fable, onyx, nova, sage, shimmer.".into(),
                ),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"voice": {"type": "string"}},
                    "required": ["voice"],
                })),
                strict: None,
            }
            .into(),
            FunctionObject {
                name: "get_voice".into(),
                description: Some("Returns the name of the current AI voice.".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                })),
                strict: None,
            }
            .into(),
        ])
        .build()?;

    let assistant = client.assistants().create(create_assistant_request).await?;

    let thread = client
        .threads()
        .create(CreateThreadRequest::default())
        .await?;

    let speak_stream = Arc::new(Mutex::new(SpeakStream::new(
        Voice::Echo,
        opt.speech_speed,
        opt.tick,
        opt.duck,
    )));

    let (audio_tx, audio_rx) = flume::unbounded();
    let interrupt_flag = Arc::new(AtomicBool::new(false));
    start_ptt_thread(
        audio_tx.clone(),
        speak_stream.clone(),
        opt.duck_ptt,
        interrupt_flag.clone(),
    );

    loop {
        let audio_path = audio_rx.recv().unwrap();
        interrupt_flag.store(false, Ordering::SeqCst);
        let transcription = transcribe::transcribe(&client, &audio_path).await?;
        println!("{}", "You: ".truecolor(0, 255, 0));
        println!("{}", transcription);

        client
            .threads()
            .messages(&thread.id)
            .create(CreateMessageRequest {
                role: MessageRole::User,
                content: transcription.into(),
                ..Default::default()
            })
            .await?;

        let mut event_stream = client
            .threads()
            .runs(&thread.id)
            .create_stream(CreateRunRequest {
                assistant_id: assistant.id.clone(),
                stream: Some(true),
                ..Default::default()
            })
            .await?;

        let speak_stream_cloned = speak_stream.clone();
        let client_cloned = client.clone();
        let mut task_handle = None;
        let mut displayed_ai_label = false;
        let mut run_id: Option<String> = None;

        while let Some(event) = event_stream.next().await {
            if interrupt_flag.swap(false, Ordering::SeqCst) {
                if let Some(id) = &run_id {
                    let _ = client.threads().runs(&thread.id).cancel(id).await;
                }
                break;
            }
            match event {
                Ok(evt) => match &evt {
                    AssistantStreamEvent::ThreadRunCreated(obj)
                    | AssistantStreamEvent::ThreadRunQueued(obj)
                    | AssistantStreamEvent::ThreadRunInProgress(obj)
                    | AssistantStreamEvent::ThreadRunRequiresAction(obj)
                    | AssistantStreamEvent::ThreadRunCompleted(obj)
                    | AssistantStreamEvent::ThreadRunFailed(obj)
                    | AssistantStreamEvent::ThreadRunCancelled(obj)
                    | AssistantStreamEvent::ThreadRunIncomplete(obj)
                    | AssistantStreamEvent::ThreadRunExpired(obj)
                    | AssistantStreamEvent::ThreadRunCancelling(obj) => {
                        if run_id.is_none() {
                            run_id = Some(obj.id.clone());
                        }
                        if matches!(evt, AssistantStreamEvent::ThreadRunRequiresAction(_)) {
                            let client = client_cloned.clone();
                            let speak_stream = speak_stream_cloned.clone();
                            let run_obj = obj.clone();
                            task_handle = Some(tokio::spawn(async move {
                                handle_requires_action(client, run_obj, speak_stream).await
                            }));
                        }
                    }
                    AssistantStreamEvent::ThreadMessageDelta(delta) => {
                        if let Some(contents) = &delta.delta.content {
                            for content in contents {
                                if let MessageDeltaContent::Text(text) = content {
                                    if let Some(text) = &text.text {
                                        if let Some(text) = &text.value {
                                            if !displayed_ai_label {
                                                println!("{}", "AI: ".truecolor(0, 0, 255));
                                                displayed_ai_label = true;
                                            }
                                            print!("{}", text);
                                            speak_stream_cloned.lock().unwrap().add_token(&text);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                },
                Err(e) => eprintln!("Error: {e}"),
            }
        }

        if let Some(handle) = task_handle {
            let _ = handle.await;
        }

        speak_stream.lock().unwrap().complete_sentence();
        println!();
    }
}

fn start_ptt_thread(
    audio_tx: Sender<PathBuf>,
    speak_stream: Arc<Mutex<SpeakStream>>,
    duck_ptt: bool,
    interrupt_flag: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut recorder = rec::Recorder::new();
        let tmp_dir = tempdir().unwrap();
        let mut key_pressed = false;
        let ptt_key = Key::F9;
        let mut current_path: Option<PathBuf> = None;
        let mut recording_start = Instant::now();

        let callback = move |event: Event| match event.event_type {
            EventType::KeyPress(key) if key == ptt_key && !key_pressed => {
                key_pressed = true;
                interrupt_flag.store(true, Ordering::SeqCst);
                {
                    let mut ss = speak_stream.lock().unwrap();
                    ss.stop_speech();
                    if duck_ptt {
                        ss.start_audio_ducking();
                    }
                }
                let path = tmp_dir.path().join(format!("{}.wav", Uuid::new_v4()));
                if recorder.start_recording(&path, None).is_ok() {
                    current_path = Some(path);
                    recording_start = Instant::now();
                }
            }
            EventType::KeyRelease(key) if key == ptt_key && key_pressed => {
                key_pressed = false;
                if recorder.stop_recording().is_ok() {
                    let elapsed = recording_start.elapsed();
                    if elapsed.as_secs_f32() >= 0.2 {
                        if let Some(p) = current_path.take() {
                            audio_tx.send(p).unwrap();
                        }
                    } else {
                        println!(
                            "{}",
                            "User recording too short. Aborting transcription and LLM response."
                                .truecolor(255, 0, 0)
                        );
                    }
                }
                if duck_ptt {
                    speak_stream.lock().unwrap().stop_audio_ducking();
                }
            }
            _ => {}
        };

        if let Err(e) = listen(callback) {
            eprintln!("Failed to listen for key events: {:?}", e);
        }
    });
}

fn get_system_info() -> String {
    let mut info = String::new();
    let mut sys = System::new_all();
    sys.refresh_all();

    info.push_str("=> system:\n");

    let total_memory = sys.total_memory();
    let used_memory = sys.used_memory();
    let total_swap = sys.total_swap();
    let used_swap = sys.used_swap();

    info.push_str(&format!("total memory: {} bytes\n", total_memory));
    info.push_str(&format!("used memory : {} bytes\n", used_memory));
    info.push_str(&format!("total swap  : {} bytes\n", total_swap));
    info.push_str(&format!("used swap   : {} bytes\n", used_swap));

    let system_name = System::name();
    let kernel_version = System::kernel_version();
    let os_version = System::os_version();
    let host_name = System::host_name();

    info.push_str(&format!("System name:             {:?}\n", system_name));
    info.push_str(&format!("System kernel version:   {:?}\n", kernel_version));
    info.push_str(&format!("System OS version:       {:?}\n", os_version));
    info.push_str(&format!("System host name:        {:?}\n", host_name));

    let nb_cpus = sys.cpus().len();
    info.push_str(&format!("NB CPUs: {}\n", nb_cpus));

    info.push_str("=> disks:\n");
    let disks = Disks::new_with_refreshed_list();
    for disk in &disks {
        info.push_str(&format!("{:?}\n", disk));
    }

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

    info.push_str("=> components:\n");
    let components = Components::new_with_refreshed_list();
    for component in &components {
        info.push_str(&format!("{:?}\n", component));
    }

    info
}

fn get_clipboard_string() -> Result<String, String> {
    let mut clipboard: ClipboardContext = ClipboardProvider::new()
        .map_err(|e| format!("Failed to initialize clipboard: {}", e))?;
    clipboard
        .get_contents()
        .map_err(|e| format!("Failed to read clipboard contents: {}", e))
}

fn speedtest() -> Result<String, String> {
    let output = match std::process::Command::new("speedtest-rs").output() {
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

async fn handle_requires_action(
    client: Client<OpenAIConfig>,
    run_object: RunObject,
    speak_stream: Arc<Mutex<SpeakStream>>,
) {
    let mut tool_outputs: Vec<ToolsOutputs> = Vec::new();

    if let Some(required_action) = &run_object.required_action {
        for tool in &required_action.submit_tool_outputs.tool_calls {
            println!("{}{}", "Invoking function: ".purple(), tool.function.name);
            if tool.function.name == "get_current_temperature" {
                tool_outputs.push(ToolsOutputs {
                    tool_call_id: Some(tool.id.clone()),
                    output: Some("57".into()),
                });
            }

            if tool.function.name == "get_rain_probability" {
                tool_outputs.push(ToolsOutputs {
                    tool_call_id: Some(tool.id.clone()),
                    output: Some("0.06".into()),
                });
            }

            if tool.function.name == "set_clipboard" {
                let mut clipboard: ClipboardContext = ClipboardProvider::new().unwrap();
                let text = match serde_json::from_str::<serde_json::Value>(&tool.function.arguments)
                {
                    Ok(v) => v["clipboard_text"].as_str().unwrap_or("").to_string(),
                    Err(_) => String::new(),
                };
                let result = clipboard.set_contents(text.clone());
                let msg = match result {
                    Ok(_) => "Clipboard set".to_string(),
                    Err(e) => format!("Failed to set clipboard: {}", e),
                };
                tool_outputs.push(ToolsOutputs {
                    tool_call_id: Some(tool.id.clone()),
                    output: Some(msg.into()),
                });
            }

            if tool.function.name == "get_clipboard" {
                let msg = match get_clipboard_string() {
                    Ok(text) => text,
                    Err(e) => e,
                };
                tool_outputs.push(ToolsOutputs {
                    tool_call_id: Some(tool.id.clone()),
                    output: Some(msg.into()),
                });
            }

            if tool.function.name == "set_screen_brightness" {
                let brightness =
                    match serde_json::from_str::<serde_json::Value>(&tool.function.arguments) {
                        Ok(v) => v["brightness"].as_i64().unwrap_or(0) as u32,
                        Err(_) => 0,
                    };

                let result = std::process::Command::new("luster")
                    .arg(brightness.to_string())
                    .output();
                let msg = match result {
                    Ok(_) => "Brightness set".to_string(),
                    Err(e) => format!("Failed to set brightness: {}", e),
                };
                tool_outputs.push(ToolsOutputs {
                    tool_call_id: Some(tool.id.clone()),
                    output: Some(msg.into()),
                });
            }

            if tool.function.name == "media_controls" {
                let button = match serde_json::from_str::<serde_json::Value>(&tool.function.arguments) {
                    Ok(v) => v["media_button"].as_str().unwrap_or("").to_string(),
                    Err(_) => String::new(),
                };

                let mut enigo = Enigo::new();
                let msg = match button.as_str() {
                    "MediaStop" => {
                        enigo.key_click(enigo::Key::MediaStop);
                        "MediaStop"
                    }
                    "MediaNextTrack" => {
                        enigo.key_click(enigo::Key::MediaNextTrack);
                        "MediaNextTrack"
                    }
                    "MediaPlayPause" => {
                        enigo.key_click(enigo::Key::MediaPlayPause);
                        "MediaPlayPause"
                    }
                    "MediaPrevTrack" => {
                        enigo.key_click(enigo::Key::MediaPrevTrack);
                        enigo.key_click(enigo::Key::MediaPrevTrack);
                        "MediaPrevTrack"
                    }
                    "VolumeUp" => {
                        for _ in 0..5 {
                            enigo.key_click(enigo::Key::VolumeUp);
                        }
                        "VolumeUp"
                    }
                    "VolumeDown" => {
                        for _ in 0..5 {
                            enigo.key_click(enigo::Key::VolumeDown);
                        }
                        "VolumeDown"
                    }
                    "VolumeMute" => {
                        enigo.key_click(enigo::Key::VolumeMute);
                        "VolumeMute"
                    }
                    _ => "Unknown button",
                };

                tool_outputs.push(ToolsOutputs {
                    tool_call_id: Some(tool.id.clone()),
                    output: Some(msg.into()),
                });
            }

            if tool.function.name == "open_openai_billing" {
                let result = open::that("https://platform.openai.com/usage");
                let msg = match result {
                    Ok(_) => "Opened OpenAI billing page".to_string(),
                    Err(e) => format!("Failed to open OpenAI billing page: {}", e),
                };
                tool_outputs.push(ToolsOutputs {
                    tool_call_id: Some(tool.id.clone()),
                    output: Some(msg.into()),
                });
            }

            if tool.function.name == "speedtest" {
                tool_outputs.push(ToolsOutputs {
                    tool_call_id: Some(tool.id.clone()),
                    output: Some(
                        "Speed test has been successfully started. It takes several seconds. The results will be shared once the speedtest is complete.".into(),
                    ),
                });

                let client = client.clone();
                let thread_id = run_object.thread_id.clone();
                let assistant_id = run_object.assistant_id.clone();
                let speak_stream = speak_stream.clone();
                tokio::spawn(async move {
                    let result = match speedtest() {
                        Ok(out) => format!("Speedtest results: {}", out),
                        Err(e) => format!("Speedtest failed with error: {}", e),
                    };

                    if let Err(e) = client
                        .threads()
                        .messages(&thread_id)
                        .create(CreateMessageRequest {
                            role: MessageRole::User,
                            content: result.clone().into(),
                            ..Default::default()
                        })
                        .await
                    {
                        eprintln!("Failed to send speedtest results: {e}");
                        return;
                    }

                    let mut event_stream = match client
                        .threads()
                        .runs(&thread_id)
                        .create_stream(CreateRunRequest {
                            assistant_id: assistant_id.unwrap_or_default(),
                            stream: Some(true),
                            ..Default::default()
                        })
                        .await
                    {
                        Ok(es) => es,
                        Err(e) => {
                            eprintln!("Failed to create run for speedtest results: {e}");
                            return;
                        }
                    };

                    let mut displayed_ai_label = false;
                    while let Some(event) = event_stream.next().await {
                        if let Ok(AssistantStreamEvent::ThreadMessageDelta(delta)) = event {
                            if let Some(contents) = &delta.delta.content {
                                for content in contents {
                                    if let MessageDeltaContent::Text(text) = content {
                                        if let Some(text) = &text.text {
                                            if let Some(text) = &text.value {
                                                if !displayed_ai_label {
                                                    println!("{}", "AI: ".truecolor(0, 0, 255));
                                                    displayed_ai_label = true;
                                                }
                                                print!("{}", text);
                                                speak_stream.lock().unwrap().add_token(text);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    speak_stream.lock().unwrap().complete_sentence();
                    println!();
                });
            }

            if tool.function.name == "set_speech_speed" {
                let speed = match serde_json::from_str::<serde_json::Value>(&tool.function.arguments) {
                    Ok(v) => v["speed"].as_f64().unwrap_or(1.0) as f32,
                    Err(_) => 1.0,
                };
                let msg = if (0.5..=100.0).contains(&speed) {
                    speak_stream.lock().unwrap().set_speech_speed(speed);
                    format!("Speech speed set to {}", speed)
                } else {
                    "Speed must be between 0.5 and 100.0".to_string()
                };
                tool_outputs.push(ToolsOutputs {
                    tool_call_id: Some(tool.id.clone()),
                    output: Some(msg.into()),
                });
            }

            if tool.function.name == "get_speech_speed" {
                let speed = speak_stream.lock().unwrap().get_speech_speed();
                tool_outputs.push(ToolsOutputs {
                    tool_call_id: Some(tool.id.clone()),
                    output: Some(format!("{}", speed).into()),
                });
            }

            if tool.function.name == "mute_speech" {
                speak_stream.lock().unwrap().mute();
                tool_outputs.push(ToolsOutputs {
                    tool_call_id: Some(tool.id.clone()),
                    output: Some("AI voice muted".into()),
                });
            }

            if tool.function.name == "unmute_speech" {
                speak_stream.lock().unwrap().unmute();
                tool_outputs.push(ToolsOutputs {
                    tool_call_id: Some(tool.id.clone()),
                    output: Some("AI voice unmuted".into()),
                });
            }

            if tool.function.name == "set_voice" {
                let name = match serde_json::from_str::<serde_json::Value>(&tool.function.arguments) {
                    Ok(v) => v["voice"].as_str().unwrap_or("").to_string(),
                    Err(_) => String::new(),
                };
                let msg = match parse_voice(&name) {
                    Some(v) => {
                        speak_stream.lock().unwrap().set_voice(v);
                        format!("Voice set to {}", name.to_lowercase())
                    }
                    None => "Invalid voice name".to_string(),
                };
                tool_outputs.push(ToolsOutputs {
                    tool_call_id: Some(tool.id.clone()),
                    output: Some(msg.into()),
                });
            }

            if tool.function.name == "get_system_info" {
                let info = get_system_info();
                tool_outputs.push(ToolsOutputs {
                    tool_call_id: Some(tool.id.clone()),
                    output: Some(info.into()),
                });
            }

            if tool.function.name == "get_voice" {
                let name = voice_to_str(&speak_stream.lock().unwrap().get_voice());
                tool_outputs.push(ToolsOutputs {
                    tool_call_id: Some(tool.id.clone()),
                    output: Some(format!("{}", name).into()),
                });
            }
        }

        if let Err(e) = submit_tool_outputs(client, run_object, tool_outputs, speak_stream).await {
            eprintln!("Error on submitting tool outputs: {e}");
        }
    }
}

async fn submit_tool_outputs(
    client: Client<OpenAIConfig>,
    run_object: RunObject,
    tool_outputs: Vec<ToolsOutputs>,
    speak_stream: Arc<Mutex<SpeakStream>>,
) -> Result<(), Box<dyn Error>> {
    let mut event_stream = client
        .threads()
        .runs(&run_object.thread_id)
        .submit_tool_outputs_stream(
            &run_object.id,
            SubmitToolOutputsRunRequest {
                tool_outputs,
                stream: Some(true),
            },
        )
        .await?;

    while let Some(event) = event_stream.next().await {
        match event {
            Ok(event) => {
                if let AssistantStreamEvent::ThreadMessageDelta(delta) = event {
                    if let Some(contents) = delta.delta.content {
                        for content in contents {
                            if let MessageDeltaContent::Text(text) = content {
                                if let Some(text) = text.text {
                                    if let Some(text) = text.value {
                                        print!("{}", text);
                                        speak_stream.lock().unwrap().add_token(&text);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => eprintln!("Error: {e}"),
        }
    }

    speak_stream.lock().unwrap().complete_sentence();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_assistant_request() {
        let req = CreateAssistantRequestArgs::default()
            .model("gpt-4o")
            .build();
        assert!(req.is_ok());
    }

    #[test]
    fn includes_open_openai_billing_function() {
        let req = CreateAssistantRequestArgs::default()
            .instructions("test")
            .model("gpt-4o")
            .tools(vec![FunctionObject {
                name: "open_openai_billing".into(),
                description: Some(
                    "Opens the OpenAI usage dashboard in the default web browser.".into(),
                ),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                })),
                strict: None,
            }
            .into()])
            .build()
            .unwrap();

        let tools = req.tools.unwrap();
        assert!(tools.iter().any(|t| match t {
            async_openai::types::AssistantTools::Function(f) =>
                f.function.name == "open_openai_billing",
            _ => false,
        }));
    }

    #[test]
    fn includes_speedtest_function() {
        let req = CreateAssistantRequestArgs::default()
            .instructions("test")
            .model("gpt-4o")
            .tools(vec![FunctionObject {
                name: "speedtest".into(),
                description: Some(
                    "Runs an internet speedtest and returns the results.".into(),
                ),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                })),
                strict: None,
            }
            .into()])
            .build()
            .unwrap();

        let tools = req.tools.unwrap();
        assert!(tools.iter().any(|t| match t {
            async_openai::types::AssistantTools::Function(f) => f.function.name == "speedtest",
            _ => false,
        }));
    }

    #[test]
    fn includes_set_screen_brightness_function() {
        let req = CreateAssistantRequestArgs::default()
            .instructions("test")
            .model("gpt-4o")
            .tools(vec![FunctionObject {
                name: "set_screen_brightness".into(),
                description: Some(
                    "Sets the screen brightness from 0 to 100 using the `luster` utility.".into(),
                ),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"brightness": {"type": "integer"}},
                    "required": ["brightness"],
                })),
                strict: None,
            }
            .into()])
            .build()
            .unwrap();

        let tools = req.tools.unwrap();
        assert!(tools.iter().any(|t| match t {
            async_openai::types::AssistantTools::Function(f) =>
                f.function.name == "set_screen_brightness",
            _ => false,
        }));
    }

    #[test]
    fn includes_set_speech_speed_function() {
        let req = CreateAssistantRequestArgs::default()
            .instructions("test")
            .model("gpt-4o")
            .tools(vec![FunctionObject {
                name: "set_speech_speed".into(),
                description: Some("Sets how fast the AI voice speaks.".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"speed": {"type": "number"}},
                    "required": ["speed"],
                })),
                strict: None,
            }
            .into()])
            .build()
            .unwrap();

        let tools = req.tools.unwrap();
        assert!(tools.iter().any(|t| match t {
            async_openai::types::AssistantTools::Function(f) =>
                f.function.name == "set_speech_speed",
            _ => false,
        }));
    }

    #[test]
    fn includes_media_controls_function() {
        let req = CreateAssistantRequestArgs::default()
            .instructions("test")
            .model("gpt-4o")
            .tools(vec![FunctionObject {
                name: "media_controls".into(),
                description: Some("Plays, pauses or seeks media.".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "media_button": {
                            "type": "string",
                            "enum": [
                                "MediaStop",
                                "MediaNextTrack",
                                "MediaPlayPause",
                                "MediaPrevTrack",
                                "VolumeUp",
                                "VolumeDown",
                                "VolumeMute"
                            ]
                        }
                    },
                    "required": ["media_button"]
                })),
                strict: None,
            }
            .into()])
            .build()
            .unwrap();

        let tools = req.tools.unwrap();
        assert!(tools.iter().any(|t| match t {
            async_openai::types::AssistantTools::Function(f) =>
                f.function.name == "media_controls",
            _ => false,
        }));
    }

    #[test]
    fn includes_mute_speech_function() {
        let req = CreateAssistantRequestArgs::default()
            .instructions("test")
            .model("gpt-4o")
            .tools(vec![FunctionObject {
                name: "mute_speech".into(),
                description: Some("Mutes the AI voice output.".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                })),
                strict: None,
            }
            .into()])
            .build()
            .unwrap();

        let tools = req.tools.unwrap();
        assert!(tools.iter().any(|t| match t {
            async_openai::types::AssistantTools::Function(f) => f.function.name == "mute_speech",
            _ => false,
        }));
    }

    #[test]
    fn includes_get_system_info_function() {
        let req = CreateAssistantRequestArgs::default()
            .instructions("test")
            .model("gpt-4o")
            .tools(vec![FunctionObject {
                name: "get_system_info".into(),
                description: Some("Returns system information like CPU and memory usage.".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                })),
                strict: None,
            }
            .into()])
            .build()
            .unwrap();

        let tools = req.tools.unwrap();
        assert!(tools.iter().any(|t| match t {
            async_openai::types::AssistantTools::Function(f) => f.function.name == "get_system_info",
            _ => false,
        }));
    }

    #[test]
    fn get_clipboard_returns_contents() {
        let mut clipboard: ClipboardContext = match ClipboardProvider::new() {
            Ok(c) => c,
            Err(_) => return,
        };
        if clipboard.set_contents("clipboard_test".to_string()).is_err() {
            return;
        }
        match get_clipboard_string() {
            Ok(contents) => assert_eq!(contents, "clipboard_test"),
            Err(_) => {}
        }
    }
}
