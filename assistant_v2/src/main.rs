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
use speakstream::ss::SpeakStream;
use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod record;
mod transcribe;
use flume::Sender;
use rdev::{listen, Event, EventType, Key};
use record::rec;
use std::thread;
use tempfile::tempdir;
use uuid::Uuid;

#[derive(Parser, Debug)]
struct Opt {
    /// How fast the AI speaks. 1.0 is normal speed.
    #[arg(long, default_value_t = 1.0)]
    speech_speed: f32,

    /// Enable ticking sound while speaking.
    #[arg(long, default_value_t = false)]
    tick: bool,

    /// Enable audio ducking while the push-to-talk key is held.
    #[arg(long, default_value_t = false)]
    duck_ptt: bool,
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
        opt.duck_ptt,
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
                                                print!("{}", "AI: ".truecolor(0, 0, 255));
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
                }
            }
            EventType::KeyRelease(key) if key == ptt_key && key_pressed => {
                key_pressed = false;
                if recorder.stop_recording().is_ok() {
                    if let Some(p) = current_path.take() {
                        audio_tx.send(p).unwrap();
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

async fn handle_requires_action(
    client: Client<OpenAIConfig>,
    run_object: RunObject,
    speak_stream: Arc<Mutex<SpeakStream>>,
) {
    let mut tool_outputs: Vec<ToolsOutputs> = Vec::new();

    if let Some(required_action) = &run_object.required_action {
        for tool in &required_action.submit_tool_outputs.tool_calls {
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
}
