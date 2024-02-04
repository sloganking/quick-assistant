use anyhow::Context;
use dotenvy::dotenv;
use std::env;
use std::fs::File;
use std::io::{stdout, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use tempfile::{tempdir, NamedTempFile};
mod transcribe;
use chrono::Local;
use futures::stream::FuturesOrdered;
use futures::stream::StreamExt; // For `.next()` on FuturesOrdered.
use std::thread;
use tempfile::Builder;
use transcribe::trans;
mod record;
use async_openai::{
    types::{
        ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
        CreateChatCompletionRequestArgs, CreateSpeechRequestArgs, SpeechModel, Voice,
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
mod speakstream;
use speakstream::speakstream as ss;

#[derive(Parser, Debug)]
#[command(version)]
struct Opt {
    /// The audio device to use for recording. Leaving this blank will use the default device.
    #[arg(short, long, default_value_t = String::from("default"))]
    device: String,

    /// Your OpenAI API key
    #[arg(short, long)]
    api_key: Option<String>,

    /// The push to talk key
    #[arg(short, long)]
    ptt_key: Option<PTTKey>,

    /// The push to talk key.
    /// Use this if you want to use a key that is not supported by the PTTKey enum.
    #[arg(short, long, conflicts_with("ptt_key"))]
    special_ptt_key: Option<u32>,

    /// How fast the AI speaks. 1.0 is normal speed.
    /// 0.5 is minimum. 100.0 is maximum.
    #[arg(short, long, default_value_t = 1.0)]
    speech_speed: f32,

    /// The voice that the AI will use to speak.
    #[arg(short, long)]
    ai_voice: Option<VoiceEnum>,

    #[clap(subcommand)]
    pub subcommands: Option<SubCommands>,
}

#[derive(Debug, Subcommand)]
pub enum SubCommands {
    /// Displays keys as you press them so you can figure out what key to use for push to talk.
    ShowKeyPresses,
    /// Lists the audio input devices on your system.
    ListDevices,
}

/// This is just a straight copy of rdev::Key, so that #[derive(clap::ValueEnum)] works.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum PTTKey {
    /// Alt key on Linux and Windows (option key on macOS)
    Alt,
    AltGr,
    Backspace,
    CapsLock,
    ControlLeft,
    ControlRight,
    Delete,
    DownArrow,
    End,
    Escape,
    F1,
    F10,
    F11,
    F12,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    Home,
    LeftArrow,
    /// also known as "windows", "super", and "command"
    MetaLeft,
    /// also known as "windows", "super", and "command"
    MetaRight,
    PageDown,
    PageUp,
    Return,
    RightArrow,
    ShiftLeft,
    ShiftRight,
    Space,
    Tab,
    UpArrow,
    PrintScreen,
    ScrollLock,
    Pause,
    NumLock,
    BackQuote,
    Num1,
    Num2,
    Num3,
    Num4,
    Num5,
    Num6,
    Num7,
    Num8,
    Num9,
    Num0,
    Minus,
    Equal,
    KeyQ,
    KeyW,
    KeyE,
    KeyR,
    KeyT,
    KeyY,
    KeyU,
    KeyI,
    KeyO,
    KeyP,
    LeftBracket,
    RightBracket,
    KeyA,
    KeyS,
    KeyD,
    KeyF,
    KeyG,
    KeyH,
    KeyJ,
    KeyK,
    KeyL,
    SemiColon,
    Quote,
    BackSlash,
    IntlBackslash,
    KeyZ,
    KeyX,
    KeyC,
    KeyV,
    KeyB,
    KeyN,
    KeyM,
    Comma,
    Dot,
    Slash,
    Insert,
    KpReturn,
    KpMinus,
    KpPlus,
    KpMultiply,
    KpDivide,
    Kp0,
    Kp1,
    Kp2,
    Kp3,
    Kp4,
    Kp5,
    Kp6,
    Kp7,
    Kp8,
    Kp9,
    KpDelete,
    Function,
    #[clap(skip)]
    Unknown(u32),
}

impl From<PTTKey> for rdev::Key {
    fn from(item: PTTKey) -> Self {
        match item {
            PTTKey::Alt => rdev::Key::Alt,
            PTTKey::AltGr => rdev::Key::AltGr,
            PTTKey::Backspace => rdev::Key::Backspace,
            PTTKey::CapsLock => rdev::Key::CapsLock,
            PTTKey::ControlLeft => rdev::Key::ControlLeft,
            PTTKey::ControlRight => rdev::Key::ControlRight,
            PTTKey::Delete => rdev::Key::Delete,
            PTTKey::DownArrow => rdev::Key::DownArrow,
            PTTKey::End => rdev::Key::End,
            PTTKey::Escape => rdev::Key::Escape,
            PTTKey::F1 => rdev::Key::F1,
            PTTKey::F10 => rdev::Key::F10,
            PTTKey::F11 => rdev::Key::F11,
            PTTKey::F12 => rdev::Key::F12,
            PTTKey::F2 => rdev::Key::F2,
            PTTKey::F3 => rdev::Key::F3,
            PTTKey::F4 => rdev::Key::F4,
            PTTKey::F5 => rdev::Key::F5,
            PTTKey::F6 => rdev::Key::F6,
            PTTKey::F7 => rdev::Key::F7,
            PTTKey::F8 => rdev::Key::F8,
            PTTKey::F9 => rdev::Key::F9,
            PTTKey::Home => rdev::Key::Home,
            PTTKey::LeftArrow => rdev::Key::LeftArrow,
            PTTKey::MetaLeft => rdev::Key::MetaLeft,
            PTTKey::MetaRight => rdev::Key::MetaRight,
            PTTKey::PageDown => rdev::Key::PageDown,
            PTTKey::PageUp => rdev::Key::PageUp,
            PTTKey::Return => rdev::Key::Return,
            PTTKey::RightArrow => rdev::Key::RightArrow,
            PTTKey::ShiftLeft => rdev::Key::ShiftLeft,
            PTTKey::ShiftRight => rdev::Key::ShiftRight,
            PTTKey::Space => rdev::Key::Space,
            PTTKey::Tab => rdev::Key::Tab,
            PTTKey::UpArrow => rdev::Key::UpArrow,
            PTTKey::PrintScreen => rdev::Key::PrintScreen,
            PTTKey::ScrollLock => rdev::Key::ScrollLock,
            PTTKey::Pause => rdev::Key::Pause,
            PTTKey::NumLock => rdev::Key::NumLock,
            PTTKey::BackQuote => rdev::Key::BackQuote,
            PTTKey::Num1 => rdev::Key::Num1,
            PTTKey::Num2 => rdev::Key::Num2,
            PTTKey::Num3 => rdev::Key::Num3,
            PTTKey::Num4 => rdev::Key::Num4,
            PTTKey::Num5 => rdev::Key::Num5,
            PTTKey::Num6 => rdev::Key::Num6,
            PTTKey::Num7 => rdev::Key::Num7,
            PTTKey::Num8 => rdev::Key::Num8,
            PTTKey::Num9 => rdev::Key::Num9,
            PTTKey::Num0 => rdev::Key::Num0,
            PTTKey::Minus => rdev::Key::Minus,
            PTTKey::Equal => rdev::Key::Equal,
            PTTKey::KeyQ => rdev::Key::KeyQ,
            PTTKey::KeyW => rdev::Key::KeyW,
            PTTKey::KeyE => rdev::Key::KeyE,
            PTTKey::KeyR => rdev::Key::KeyR,
            PTTKey::KeyT => rdev::Key::KeyT,
            PTTKey::KeyY => rdev::Key::KeyY,
            PTTKey::KeyU => rdev::Key::KeyU,
            PTTKey::KeyI => rdev::Key::KeyI,
            PTTKey::KeyO => rdev::Key::KeyO,
            PTTKey::KeyP => rdev::Key::KeyP,
            PTTKey::LeftBracket => rdev::Key::LeftBracket,
            PTTKey::RightBracket => rdev::Key::RightBracket,
            PTTKey::KeyA => rdev::Key::KeyA,
            PTTKey::KeyS => rdev::Key::KeyS,
            PTTKey::KeyD => rdev::Key::KeyD,
            PTTKey::KeyF => rdev::Key::KeyF,
            PTTKey::KeyG => rdev::Key::KeyG,
            PTTKey::KeyH => rdev::Key::KeyH,
            PTTKey::KeyJ => rdev::Key::KeyJ,
            PTTKey::KeyK => rdev::Key::KeyK,
            PTTKey::KeyL => rdev::Key::KeyL,
            PTTKey::SemiColon => rdev::Key::SemiColon,
            PTTKey::Quote => rdev::Key::Quote,
            PTTKey::BackSlash => rdev::Key::BackSlash,
            PTTKey::IntlBackslash => rdev::Key::IntlBackslash,
            PTTKey::KeyZ => rdev::Key::KeyZ,
            PTTKey::KeyX => rdev::Key::KeyX,
            PTTKey::KeyC => rdev::Key::KeyC,
            PTTKey::KeyV => rdev::Key::KeyV,
            PTTKey::KeyB => rdev::Key::KeyB,
            PTTKey::KeyN => rdev::Key::KeyN,
            PTTKey::KeyM => rdev::Key::KeyM,
            PTTKey::Comma => rdev::Key::Comma,
            PTTKey::Dot => rdev::Key::Dot,
            PTTKey::Slash => rdev::Key::Slash,
            PTTKey::Insert => rdev::Key::Insert,
            PTTKey::KpReturn => rdev::Key::KpReturn,
            PTTKey::KpMinus => rdev::Key::KpMinus,
            PTTKey::KpPlus => rdev::Key::KpPlus,
            PTTKey::KpMultiply => rdev::Key::KpMultiply,
            PTTKey::KpDivide => rdev::Key::KpDivide,
            PTTKey::Kp0 => rdev::Key::Kp0,
            PTTKey::Kp1 => rdev::Key::Kp1,
            PTTKey::Kp2 => rdev::Key::Kp2,
            PTTKey::Kp3 => rdev::Key::Kp3,
            PTTKey::Kp4 => rdev::Key::Kp4,
            PTTKey::Kp5 => rdev::Key::Kp5,
            PTTKey::Kp6 => rdev::Key::Kp6,
            PTTKey::Kp7 => rdev::Key::Kp7,
            PTTKey::Kp8 => rdev::Key::Kp8,
            PTTKey::Kp9 => rdev::Key::Kp9,
            PTTKey::KpDelete => rdev::Key::KpDelete,
            PTTKey::Function => rdev::Key::Function,
            PTTKey::Unknown(code) => rdev::Key::Unknown(code),
        }
    }
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum VoiceEnum {
    Alloy,
    Echo,
    Fable,
    Onyx,
    Nova,
    Shimmer,
    #[clap(skip)]
    Other(String),
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
            VoiceEnum::Other(string) => Voice::Other(string),
        }
    }
}

fn println_error(err: &str) {
    println!("{}: {}", "Error".truecolor(255, 0, 0), err);
}

/// Speeds up an audio file by a factor of `speed`.
fn adjust_audio_file_speed(input: &Path, output: &Path, speed: f32) {
    // ffmpeg -y -i input.mp3 -filter:a "atempo={speed}" -vn output.mp3
    match Command::new("ffmpeg")
        .args([
            "-y",
            "-i",
            input
                .to_str()
                .context("Failed to convert input path to string")
                .unwrap(),
            // -codec:a libmp3lame -b:a 160k
            // audio quality decreases from 160k bitrate to 33k bitrate without these lines.
            "-codec:a",
            "libmp3lame",
            "-b:a",
            "160k",
            //
            "-filter:a",
            format!("atempo={}", speed).as_str(),
            "-vn",
            output
                .to_str()
                .context("Failed to convert output path to string")
                .unwrap(),
        ])
        .output()
    {
        Ok(x) => {
            if !x.status.success() {
                panic!("ffmpeg failed to adjust audio speed");
            }
            x
        }
        Err(err) => {
            if err.kind() == std::io::ErrorKind::NotFound {
                panic!("ffmpeg not found. Please install ffmpeg and add it to your PATH");
            } else {
                panic!("ffmpeg failed to adjust audio speed");
            }
        }
    };
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

fn get_second_to_last_char(s: &str) -> Option<char> {
    s.chars().rev().nth(1)
}

struct SentenceAccumulator {
    buffer: String,
    sentence_end_chars: Vec<char>,
    ai_voice_channel_tx: flume::Sender<String>,
}

impl SentenceAccumulator {
    fn new(channel: flume::Sender<String>) -> Self {
        SentenceAccumulator {
            buffer: String::new(),
            sentence_end_chars: vec!['.', '?', '!'],
            ai_voice_channel_tx: channel,
        }
    }

    fn add_token(&mut self, token: &str) {
        for char in token.chars() {
            self.buffer.push(char);

            if self.buffer.len() > 50 {
                if let Some(second_to_last_char) = get_second_to_last_char(&self.buffer) {
                    if
                    // If the second to last character is a sentence ending character
                    self.sentence_end_chars.contains(&second_to_last_char)
                    // and the last character is whitespace.
                        && self
                            .buffer
                            .chars()
                            .last()
                            .map_or(false, |c| c.is_whitespace())
                    {
                        self.process_sentence();
                        self.buffer.clear();
                    }
                }
            }
        }
    }

    fn process_sentence(&self) {
        // Trim the sentence before processing to remove any leading or trailing whitespace.
        let sentence = self.buffer.trim();
        if !sentence.is_empty() {
            // println!("\n{}{}", "Complete sentence: ".yellow(), sentence);

            // Turn the sentence into speech
            self.ai_voice_channel_tx.send(sentence.to_string()).unwrap();
        }
    }

    /// Called at the end of the conversation to process the last sentence.
    /// This is necessary since the last character may not be whitespace preceded
    /// by a sentence ending character.
    fn complete_sentence(&mut self) {
        self.process_sentence();
        self.buffer.clear();
    }

    fn clear_buffer(&mut self) {
        self.buffer.clear();
    }
}

async fn turn_text_to_speech(ai_text: String, speed: f32) -> Option<NamedTempFile> {
    let client = Client::new();

    // Turn AI's response into speech

    let request = CreateSpeechRequestArgs::default()
        .input(ai_text)
        .voice(Voice::Echo)
        .model(SpeechModel::Tts1)
        .build()
        .unwrap();

    let response =
        match future::timeout(Duration::from_secs(15), client.audio().speech(request)).await {
            Ok(transcription_result) => transcription_result,
            Err(err) => {
                println_error(&format!(
                    "Failed to turn text to speech due to timeout: {:?}",
                    err
                ));

                // play_audio(&failed_temp_file.path());

                // continue;
                return None;
            }
        }
        .unwrap();

    let ai_speech_segment_tempfile = Builder::new()
        .prefix("ai-speech-segment")
        .suffix(".mp3")
        .rand_bytes(16)
        .tempfile()
        .unwrap();

    let _ = match future::timeout(
        Duration::from_secs(10),
        response.save(ai_speech_segment_tempfile.path()),
    )
    .await
    {
        Ok(transcription_result) => transcription_result,
        Err(err) => {
            println_error(&format!(
                "Failed to save ai speech to file due to timeout: {:?}",
                err
            ));

            // play_audio(&failed_temp_file.path());

            // continue;
            return None;
        }
    };

    if speed != 1.0 {
        let sped_up_audio_path = Builder::new()
            .prefix("quick-assist-ai-voice-sped-up")
            .suffix(".mp3")
            .rand_bytes(16)
            .tempfile()
            .unwrap();

        adjust_audio_file_speed(
            ai_speech_segment_tempfile.path(),
            sped_up_audio_path.path(),
            speed as f32,
        );

        return Some(sped_up_audio_path);
    }

    Some(ai_speech_segment_tempfile)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let opt = Opt::parse();
    let _ = dotenv();

    let (speak_stream, stream) = ss::SpeakStream::new();
    let speak_stream_mutex = Arc::new(Mutex::new(speak_stream));

    let (audio_playing_tx, audio_playing_rx): (flume::Sender<PathBuf>, flume::Receiver<PathBuf>) =
        flume::unbounded();

    let (ai_tts_tx, ai_tts_rx): (flume::Sender<String>, flume::Receiver<String>) =
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

            // // Setup vars for playing sound through speakers
            // let (_stream, stream_handle) = rodio::OutputStream::try_default().unwrap();
            // let ai_voice_sink = rodio::Sink::try_new(&stream_handle).unwrap();
            // let ai_voice_sink = Arc::new(ai_voice_sink);

            // let (ai_audio_playing_tx, ai_audio_playing_rx): (
            //     flume::Sender<NamedTempFile>,
            //     flume::Receiver<NamedTempFile>,
            // ) = flume::unbounded();

            // let sentence_accumulator = Arc::new(Mutex::new(SentenceAccumulator::new(ai_tts_tx)));

            // let futures_ordered_mutex = Arc::new(Mutex::new(FuturesOrdered::new()));

            let llm_should_stop_mutex = Arc::new(Mutex::new(false));

            // let thread_ai_voice_sink = ai_voice_sink.clone();

            // let thread_ai_audio_playing_rx = ai_audio_playing_rx.clone();

            // Create audio recorder thread
            // This thread listens to the push to talk key and records audio when it's pressed.
            // It then sends the path of the recorded audio file to the AI thread.
            // let thread_sentence_accumulator_mutex = sentence_accumulator.clone();
            let thread_ai_tts_rx = ai_tts_rx.clone();
            // let thread_futures_ordered_mutex = futures_ordered_mutex.clone();
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
                                    Ok(_) => (),
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
                                        continue;
                                    }
                                };
                                match recorder.stop_recording() {
                                    Ok(_) => (),
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
                                    println_error("Recording too short");
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

            // let thread_ai_voice_sink = ai_voice_sink.clone();

            // Create AI thread
            // This thread listens to the audio recorder thread and transcribes the audio
            // before feeding it to the AI assistant.
            // let thread_sentence_accumulator_mutex = sentence_accumulator.clone();
            let thread_llm_should_stop_mutex = llm_should_stop_mutex.clone();
            let thread_speak_stream_mutex = speak_stream_mutex.clone();
            thread::spawn(move || {
                let client = Client::new();
                let mut message_history: Vec<ChatCompletionRequestMessage> = Vec::new();

                // let mut sentence_accumulator = SentenceAccumulator::new(ai_tts_tx);

                message_history.push(
                    ChatCompletionRequestSystemMessageArgs::default()
                        .content("You are a desktop voice assistant. Your responses will be spoken by a text to speech engine. You should be helpful but concise. As conversations should be a back and forth. Don't make audio clips that run on for more than 15 seconds. Also don't ask 'if I would like to know more'")
                        .build()
                        .unwrap()
                        .into(),
                );

                let runtime = tokio::runtime::Runtime::new()
                    .context("Failed to create tokio runtime")
                    .unwrap();

                for audio_path in recording_rx.iter() {
                    // thread_ai_voice_sink.stop();

                    let mut thread_speak_stream = thread_speak_stream_mutex.lock().unwrap();
                    thread_speak_stream.stop_speech();
                    drop(thread_speak_stream);

                    let transcription_result = match runtime.block_on(future::timeout(
                        Duration::from_secs(10),
                        trans::transcribe(&client, &audio_path),
                    )) {
                        Ok(transcription_result) => transcription_result,
                        Err(err) => {
                            println_error(&format!(
                                "Failed to transcribe audio due to timeout: {:?}",
                                err
                            ));

                            play_audio(&failed_temp_file.path());

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
                        continue;
                    }

                    println!("{}", "You: ".truecolor(0, 255, 0));
                    println!("{}", transcription);

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
                            .model("gpt-4-1106-preview")
                            .max_tokens(512u16)
                            .messages(message_history.clone())
                            .build()
                            .unwrap();

                        let mut stream = runtime
                            .block_on(client.chat().create_stream(request))
                            .unwrap();

                        // let test = ;

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

                                    play_audio(&failed_temp_file.path());

                                    break 'request;
                                }
                            }
                        } {
                            // println!("result: {:#?}",result);

                            let mut llm_should_stop = thread_llm_should_stop_mutex.lock().unwrap();

                            if *llm_should_stop {
                                *llm_should_stop = false;

                                // remember what the AI said so far.
                                message_history.push(
                                    ChatCompletionRequestAssistantMessageArgs::default()
                                        .content(&ai_content)
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
                                    response.choices.iter().for_each(|chat_choice| {
                                        if let Some(ref content) = chat_choice.delta.content {
                                            if !displayed_ai_label {
                                                // writeln!(lock, "{}", "AI: ".truecolor(255, 0, 0))
                                                //     .unwrap();
                                                println!("{}", "AI: ".truecolor(0, 0, 255));
                                                displayed_ai_label = true;
                                            }
                                            // write!(lock, "{}", content).unwrap();
                                            print!("{}", content);
                                            ai_content += content;
                                            // sentence_accumulator.add_token(&content);

                                            // let mut thread_sentence_accumulator =
                                            //     thread_sentence_accumulator_mutex.lock().unwrap();
                                            // thread_sentence_accumulator.add_token(&content);
                                            // drop(thread_sentence_accumulator);

                                            let mut thread_speak_stream =
                                                thread_speak_stream_mutex.lock().unwrap();
                                            thread_speak_stream.add_token(&content);
                                            drop(thread_speak_stream);
                                        }
                                    });
                                }
                                Err(_err) => {
                                    // writeln!(lock, "error: {_err}").unwrap();
                                    println!("error: {_err}");
                                    if !message_history.is_empty() {
                                        message_history.remove(0);
                                        // writeln!(
                                        //     lock,
                                        //     "{}Removed message. There are now {} remembered messages",
                                        //     "System: ".truecolor(255, 165, 0),
                                        //     message_history.len()
                                        // )
                                        // .unwrap();
                                        println!(
                                            "Removed message. There are now {} remembered messages",
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
                                .content(&ai_content)
                                // .role(Role::Assistant)
                                .build()
                                .unwrap()
                                .into(),
                        );
                        break;
                    }

                    // Tells the ai voice to speak the remaining text in the buffer
                    // sentence_accumulator.complete_sentence();

                    // let mut thread_sentence_accumulator =
                    //     thread_sentence_accumulator_mutex.lock().unwrap();
                    // thread_sentence_accumulator.complete_sentence();
                    // drop(thread_sentence_accumulator);

                    let mut thread_speak_stream = thread_speak_stream_mutex.lock().unwrap();
                    thread_speak_stream.complete_sentence();
                    drop(thread_speak_stream);
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

            // // Create text to speech conversion thread
            // // that will convert text to speech and pass the audio file path to
            // // the ai voice audio playing thread
            // let thread_futures_ordered_mutex = futures_ordered_mutex.clone();
            // thread::spawn(move || {
            //     let runtime = tokio::runtime::Runtime::new()
            //         .context("Failed to create tokio runtime")
            //         .unwrap();

            //     loop {
            //         let mut futures_ordered = thread_futures_ordered_mutex.lock().unwrap();

            //         // Queue up any text segments to be turned into speech.
            //         for ai_text in ai_tts_rx.try_iter() {
            //             futures_ordered.push_back(turn_text_to_speech(ai_text, opt.speech_speed));
            //         }

            //         // Send any ready audio segments to the ai voice audio playing thread
            //         while let Some(tempfile_option) = runtime.block_on(futures_ordered.next()) {
            //             match tempfile_option {
            //                 Some(tempfile) => {
            //                     // send tempfile to ai voice audio playing thread
            //                     ai_audio_playing_tx.send(tempfile).unwrap();
            //                 }
            //                 None => {
            //                     // play_audio(&failed_temp_file.path());
            //                     println_error("failed to turn text to speech");
            //                 }
            //             }
            //         }
            //         drop(futures_ordered);
            //     }
            // });

            // // Create the ai voice audio playing thread
            // let thread_ai_voice_sink = ai_voice_sink.clone();
            // let thread_ai_audio_playing_rx = ai_audio_playing_rx.clone();
            // tokio::spawn(async move {
            //     // println!("Waiting for ai_voice_playing_rx");
            //     for ai_speech_segment in thread_ai_audio_playing_rx.iter() {
            //         // play the sound of AI speech
            //         let file = std::fs::File::open(ai_speech_segment.path()).unwrap();
            //         thread_ai_voice_sink.stop();
            //         thread_ai_voice_sink.append(rodio::Decoder::new(BufReader::new(file)).unwrap());
            //         // sink.play();

            //         thread_ai_voice_sink.sleep_until_end();
            //     }
            // });

            // Have this main thread recieve events and send them to the key handler thread
            {
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
