pub mod ss {

    use anyhow::{anyhow, Context, Result};
    use async_openai::{
        types::{CreateSpeechRequestArgs, SpeechModel, Voice},
        Client,
    };
    use async_std::future;
    use colored::Colorize;
    use futures::{future::FutureExt, select};
    use rodio::OutputStream;
    use std::io::BufReader;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;
    use tempfile::{Builder, NamedTempFile};
    use tokio::task;

    use tracing::{debug, info, warn};

    use crate::truncate;
    // Removed: use crate::error_and_panic;

    fn println_error(err: &str) {
        println!("{}: {}", "Error".truecolor(255, 0, 0), err);
        warn!("{}", err);
    }

    #[derive(Debug)]
    enum TtsResult {
        Ok(NamedTempFile, String),
        Err(String, String),
    }

    const ERROR_SOUND_PATH: &str = "error_sound.mp3";

    struct SentenceAccumulator {
        buffer: String,
        sentence_end_chars: Vec<char>,
    }

    impl SentenceAccumulator {
        fn new() -> Self {
            SentenceAccumulator {
                buffer: String::new(),
                sentence_end_chars: vec!['.', '?', '!'],
            }
        }

        fn add_token(&mut self, token: &str) -> Vec<String> {
            let mut sentences: Vec<String> = Vec::new();
            for char in token.chars() {
                self.buffer.push(char);

                if self.buffer.len() > 300 {
                    let sentence = self.buffer.trim();
                    if !sentence.is_empty() {
                        sentences.push(sentence.to_string());
                    }
                    self.buffer.clear();
                } else if self.buffer.len() > 200
                    && self
                        .buffer
                        .chars()
                        .last()
                        .map_or(false, |c| c.is_whitespace())
                {
                    let sentence = self.buffer.trim();
                    if !sentence.is_empty() {
                        sentences.push(sentence.to_string());
                    }
                    self.buffer.clear();
                } else if self.buffer.len() > 15 {
                    if let Some(second_to_last_char) = get_second_to_last_char(&self.buffer) {
                        if self.sentence_end_chars.contains(&second_to_last_char)
                            && self
                                .buffer
                                .chars()
                                .last()
                                .map_or(false, |c| c.is_whitespace())
                        {
                            let sentence = self.buffer.trim();
                            if !sentence.is_empty() {
                                sentences.push(sentence.to_string());
                            }
                            self.buffer.clear();
                        }
                    }
                }
            }
            sentences
        }

        fn complete_sentence(&mut self) -> Option<String> {
            let sentence = self.buffer.trim();
            let sentence_option = if !sentence.is_empty() {
                Some(sentence.to_string())
            } else {
                None
            };
            self.buffer.clear();
            sentence_option
        }

        fn clear_buffer(&mut self) {
            self.buffer.clear();
        }
    }

    fn adjust_audio_file_speed(input: &Path, output: &Path, speed: f32) -> Result<()> {
        let output_result = Command::new("ffmpeg")
            .args([
                "-y",
                "-i",
                input
                    .to_str()
                    .context("Failed to convert input path to string")?,
                "-codec:a",
                "libmp3lame",
                "-b:a",
                "160k",
                "-filter:a",
                &format!("atempo={}", speed),
                "-vn",
                output
                    .to_str()
                    .context("Failed to convert output path to string")?,
            ])
            .output();

        match output_result {
            Ok(x) => {
                if !x.status.success() {
                    Err(anyhow!("ffmpeg failed to adjust audio speed"))
                } else {
                    Ok(())
                }
            }
            Err(err) => {
                if err.kind() == std::io::ErrorKind::NotFound {
                    Err(anyhow!(
                        "ffmpeg not found. Please install ffmpeg and add it to your PATH"
                    ))
                } else {
                    Err(anyhow!("ffmpeg failed to adjust audio speed: {}", err))
                }
            }
        }
    }

    async fn turn_text_to_speech(ai_text: String, speed: f32, voice: Voice) -> TtsResult {
        let client = Client::new();

        let request = match CreateSpeechRequestArgs::default()
            .input(&ai_text)
            .voice(voice.clone())
            .model(SpeechModel::Tts1)
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                return TtsResult::Err(
                    ai_text.clone(),
                    format!("Failed to build TTS request: {}", e),
                );
            }
        };

        let response =
            match future::timeout(Duration::from_secs(15), client.audio().speech(request)).await {
                Ok(Ok(resp)) => resp,
                Ok(Err(e)) => {
                    return TtsResult::Err(
                        ai_text.clone(),
                        format!("OpenAI TTS request failed: {}", e),
                    );
                }
                Err(err) => {
                    return TtsResult::Err(
                        ai_text.clone(),
                        format!("Failed to turn text to speech due to timeout: {:?}", err),
                    );
                }
            };

        let ai_speech_segment_tempfile = match Builder::new()
            .prefix("ai-speech-segment")
            .suffix(".mp3")
            .rand_bytes(16)
            .tempfile()
        {
            Ok(file) => file,
            Err(e) => {
                return TtsResult::Err(
                    ai_text.clone(),
                    format!("Failed to create tempfile for TTS: {}", e),
                );
            }
        };

        if let Err(err) = future::timeout(
            Duration::from_secs(10),
            response.save(ai_speech_segment_tempfile.path()),
        )
        .await
        {
            return TtsResult::Err(
                ai_text.clone(),
                format!("Failed to save AI speech to file due to timeout: {:?}", err),
            );
        }

        if (speed - 1.0).abs() > f32::EPSILON {
            let sped_up_audio_path = match Builder::new()
                .prefix("quick-assist-ai-voice-sped-up")
                .suffix(".mp3")
                .rand_bytes(16)
                .tempfile()
            {
                Ok(file) => file,
                Err(e) => {
                    return TtsResult::Err(
                        ai_text.clone(),
                        format!("Failed to create tempfile for sped-up TTS: {}", e),
                    );
                }
            };

            if let Err(e) = adjust_audio_file_speed(
                ai_speech_segment_tempfile.path(),
                sped_up_audio_path.path(),
                speed,
            ) {
                return TtsResult::Err(ai_text, format!("Adjusting audio speed failed: {}", e));
            }

            TtsResult::Ok(sped_up_audio_path, ai_text)
        } else {
            TtsResult::Ok(ai_speech_segment_tempfile, ai_text)
        }
    }

    fn get_second_to_last_char(s: &str) -> Option<char> {
        s.chars().rev().nth(1)
    }

    /// SpeakStream no longer stores `ai_audio_playing_rx` because we move it into the thread.
    pub struct SpeakStream {
        sentence_accumulator: SentenceAccumulator,
        ai_tts_tx: flume::Sender<String>,
        ai_tts_rx: flume::Receiver<String>,
        futures_ordered_kill_tx: flume::Sender<()>,
        stop_speech_tx: flume::Sender<()>,
        // Removed `ai_audio_playing_rx` to avoid E0382 (move errors).
    }

    impl SpeakStream {
        pub fn new(voice: Voice, speech_speed: f32) -> (Self, OutputStream) {
            const AI_VOICE_SINK_BUFFER_SIZE: usize = 10;

            let (ai_tts_tx, ai_tts_rx) = flume::unbounded();
            let (stop_speech_tx, stop_speech_rx) = flume::unbounded();
            let (_stream, stream_handle) = rodio::OutputStream::try_default().unwrap();

            // This is the channel used for sending TtsResult to the audio thread.
            let (ai_audio_playing_tx, ai_audio_playing_rx) =
                flume::bounded(AI_VOICE_SINK_BUFFER_SIZE);

            let (futures_ordered_kill_tx, futures_ordered_kill_rx) = flume::unbounded();

            // ---------------------------------------------------------------------------
            // 1) TTS conversion task
            // ---------------------------------------------------------------------------
            // let thread_ai_tts_rx = ai_tts_rx.clone();
            let thread_ai_tts_rx: flume::Receiver<String> = ai_tts_rx.clone();

            let thread_voice = voice.clone();
            tokio::spawn(async move {
                let (converting_tx, converting_rx) = flume::bounded(AI_VOICE_SINK_BUFFER_SIZE);

                // Producer: read text from ai_tts_rx, spawn the TTS future, push to converting_tx
                {
                    let converting_tx = converting_tx.clone();
                    tokio::spawn(async move {
                        while let Ok(ai_text) = thread_ai_tts_rx.recv_async().await {
                            let thread_voice = thread_voice.clone();
                            let thread_ai_text = ai_text.clone();
                            let handle = tokio::spawn(async move {
                                turn_text_to_speech(thread_ai_text, speech_speed, thread_voice)
                                    .await
                            });

                            converting_tx.send_async(handle).await.unwrap();
                            debug!(
                                "Sent TTS conversion request with text: \"{}\"",
                                truncate(&ai_text, 20)
                            );
                        }
                    });
                }

                // Consumer: poll the converting_rx, await each handle, and send the result
                loop {
                    tokio::time::sleep(Duration::from_millis(100)).await;

                    // If we get a kill signal, abort all pending jobs
                    for _ in futures_ordered_kill_rx.try_iter() {
                        while let Ok(handle) = converting_rx.try_recv() {
                            handle.abort();
                        }
                    }

                    // Process available completed TTS futures
                    while let Ok(handle) = converting_rx.try_recv() {
                        let tts_result = match handle.await {
                            Ok(r) => r,
                            Err(join_err) => TtsResult::Err(
                                "[unknown text]".into(),
                                format!("TTS task panicked: {}", join_err),
                            ),
                        };

                        let mut kill_signal_sent = false;
                        for _ in futures_ordered_kill_rx.try_iter() {
                            while let Ok(h) = converting_rx.try_recv() {
                                h.abort();
                            }
                            kill_signal_sent = true;
                        }

                        if !kill_signal_sent {
                            // Send TTS result (Ok or Err) to the audio-playing thread
                            ai_audio_playing_tx.send(tts_result).unwrap();
                        }
                    }
                }
            });

            // ---------------------------------------------------------------------------
            // 2) Audio-playing thread
            // ---------------------------------------------------------------------------
            thread::spawn(move || {
                // Create a Tokio runtime for blocking until end
                let runtime = tokio::runtime::Runtime::new()
                    .context("Failed to create tokio runtime")
                    .unwrap();

                // Create *one* initial OutputStream (not strictly necessary to create it here,
                // but we do so just to mirror your old approach).
                let (mut _stream, _stream_handle) = rodio::OutputStream::try_default().unwrap();
                let sink = rodio::Sink::try_new(&_stream_handle).unwrap();
                let mut ai_voice_sink = Arc::new(sink);

                for tts_result in ai_audio_playing_rx.iter() {
                    // 1. Create a brand-new OutputStream + Sink *each* time (old approach).
                    let (new_stream, new_handle) = rodio::OutputStream::try_default().unwrap();
                    let new_sink = rodio::Sink::try_new(&new_handle).unwrap();

                    // Overwrite the older references with new ones
                    ai_voice_sink = Arc::new(new_sink);
                    _stream = new_stream;

                    match tts_result {
                        TtsResult::Ok(tempfile, text) => {
                            // Open the TTS file
                            let file = std::fs::File::open(tempfile.path()).unwrap();

                            // Stop the sink, just like old code, then append
                            // (Though strictly speaking, `stop()` might not be necessary on a brand-new sink,
                            // but we replicate your old code exactly.)
                            ai_voice_sink.stop();

                            ai_voice_sink.append(
                                rodio::Decoder::new(std::io::BufReader::new(file)).unwrap(),
                            );

                            info!("Playing AI voice audio: \"{}\"", truncate(&text, 20));
                        }
                        TtsResult::Err(text, error_msg) => {
                            // Log the error
                            println_error(&format!(
                                "Failed to speak text: \"{}\" due to error: {}",
                                text, error_msg
                            ));
                            // Attempt to open & play error sound file
                            match std::fs::File::open(ERROR_SOUND_PATH) {
                                Ok(file) => {
                                    ai_voice_sink.stop();
                                    ai_voice_sink.append(
                                        rodio::Decoder::new(std::io::BufReader::new(file)).unwrap(),
                                    );
                                    info!(
                                        "Playing error sound for text: \"{}\"",
                                        truncate(&text, 20)
                                    );
                                }
                                Err(open_err) => {
                                    println_error(&format!(
                                        "Could not open error sound file '{}': {}",
                                        ERROR_SOUND_PATH, open_err
                                    ));
                                }
                            }
                        }
                    }

                    // 2. Drain any leftover stop_speech messages
                    //    (Matches your old code's "while stop_speech_rx.try_recv().is_ok() {}" line.)
                    while stop_speech_rx.try_recv().is_ok() {}

                    // 3. Block until audio finishes or we receive a stop signal
                    runtime.block_on(async {
                        let blocking_task = {
                            let ai_voice_sink = ai_voice_sink.clone();

                            // We'll use spawn_blocking so that .sleep_until_end() doesn't block the entire runtime
                            task::spawn_blocking(move || {
                                ai_voice_sink.sleep_until_end();
                            })
                        };

                        select! {
                            _ = blocking_task.fuse() => {
                                // The audio finished naturally.
                            },
                            _ = stop_speech_rx.recv_async() => {
                                // If we got a stop signal, drain the channel for any additional signals
                                while stop_speech_rx.try_recv().is_ok() {}

                                // Then stop the sink
                                ai_voice_sink.stop();
                            }
                        }
                    });
                }
            });

            // Finally, return the SpeakStream and the underlying _stream
            (
                SpeakStream {
                    sentence_accumulator: SentenceAccumulator::new(),
                    ai_tts_tx,
                    ai_tts_rx,
                    futures_ordered_kill_tx,
                    stop_speech_tx,
                },
                _stream,
            )
        }

        pub fn add_token(&mut self, token: &str) {
            let sentences = self.sentence_accumulator.add_token(token);
            for sentence in sentences {
                self.ai_tts_tx.send(sentence).unwrap();
            }
        }

        pub fn complete_sentence(&mut self) {
            if let Some(sentence) = self.sentence_accumulator.complete_sentence() {
                self.ai_tts_tx.send(sentence).unwrap();
            }
        }

        pub fn stop_speech(&mut self) {
            // Clear out any partial sentences
            self.sentence_accumulator.clear_buffer();

            // Drain any queued text messages awaiting TTS
            for _ in self.ai_tts_rx.try_iter() {}

            // Kill all conversions in progress
            self.futures_ordered_kill_tx.send(()).unwrap();

            // We do NOT try to drain `ai_audio_playing_rx` here,
            // because we've already moved that receiver to the thread above.

            // Stop the current playback
            self.stop_speech_tx.send(()).unwrap();
        }
    }
}
