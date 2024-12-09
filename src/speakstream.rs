pub mod ss {

    use anyhow::Context;
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
    use tempfile::Builder;
    use tempfile::NamedTempFile;
    use tokio::task;
    use tracing::debug;
    use tracing::error;
    use tracing::info;

    use crate::error_and_panic;
    use crate::truncate;

    fn println_error(err: &str) {
        println!("{}: {}", "Error".truecolor(255, 0, 0), err);
    }

    /// SentenceAccumulator is a struct that accumulates tokens into sentences
    /// before sending the sentences to the AI voice channel.
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

        /// Adds a token to the sentence accumulator.
        /// Returns a vector of sentences that have been completed.
        fn add_token(&mut self, token: &str) -> Vec<String> {
            let mut sentences: Vec<String> = Vec::new();
            for char in token.chars() {
                self.buffer.push(char);

                if self.buffer.len() > 300 {
                    // Push the sentence to the sentences vector and clear the buffer
                    {
                        let sentence = self.buffer.trim();
                        if !sentence.is_empty() {
                            sentences.push(sentence.to_string());
                        }
                        self.buffer.clear();
                    }
                } else if self.buffer.len() > 200
                    && self
                        .buffer
                        .chars()
                        .last()
                        .map_or(false, |c| c.is_whitespace())
                {
                    // Push the sentence to the sentences vector and clear the buffer
                    {
                        let sentence = self.buffer.trim();
                        if !sentence.is_empty() {
                            sentences.push(sentence.to_string());
                        }
                        self.buffer.clear();
                    }
                } else if self.buffer.len() > 15 {
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
                            // Push the sentence to the sentences vector and clear the buffer
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
            }

            sentences
        }

        /// Called at the end of the conversation to process the last sentence.
        /// This is necessary since the last character may not be whitespace preceded
        /// by a sentence ending character.
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
                    error_and_panic("ffmpeg failed to adjust audio speed");
                }
                x
            }
            Err(err) => {
                if err.kind() == std::io::ErrorKind::NotFound {
                    error_and_panic(
                        "ffmpeg not found. Please install ffmpeg and add it to your PATH",
                    );
                } else {
                    error_and_panic("ffmpeg failed to adjust audio speed");
                }
            }
        };
    }

    /// Turns text into speech using the AI voice.
    async fn turn_text_to_speech(
        ai_text: String,
        speed: f32,
        voice: Voice,
    ) -> Option<(NamedTempFile, String)> {
        let client = Client::new();

        // Turn AI's response into speech

        let request = CreateSpeechRequestArgs::default()
            .input(&ai_text)
            .voice(voice.clone())
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
                speed,
            );

            Some((sped_up_audio_path, ai_text))
        } else {
            Some((ai_speech_segment_tempfile, ai_text))
        }
    }

    fn get_second_to_last_char(s: &str) -> Option<char> {
        s.chars().rev().nth(1)
    }

    /// SpeakStream is a struct that accumulates tokens into sentences
    /// Once a sentence is complete, it speaks the sentence using the AI voice.
    pub struct SpeakStream {
        sentence_accumulator: SentenceAccumulator,
        ai_tts_tx: flume::Sender<String>,
        ai_tts_rx: flume::Receiver<String>,
        futures_ordered_kill_tx: flume::Sender<()>,
        stop_speech_tx: flume::Sender<()>,
        ai_audio_playing_rx: flume::Receiver<(NamedTempFile, String)>,
    }

    impl SpeakStream {
        pub fn new(voice: Voice, speech_speed: f32) -> (Self, OutputStream) {
            // The maximum number of audio files that can be queued up to be played by the AI voice audio
            // playing thread Limiting this number prevents converting too much text to speech at once and
            // incurring large API costs for conversions that may not be used if speaking is stopped.
            const AI_VOICE_SINK_BUFFER_SIZE: usize = 10;

            // The sentence accumulator sends sentences to this channel to be turned into speech audio
            let (ai_tts_tx, ai_tts_rx): (flume::Sender<String>, flume::Receiver<String>) =
                flume::unbounded();

            let (stop_speech_tx, stop_speech_rx): (flume::Sender<()>, flume::Receiver<()>) =
                flume::unbounded();

            // Create the AI voice audio sink
            let (_stream, stream_handle) = rodio::OutputStream::try_default().unwrap();
            let ai_voice_sink = rodio::Sink::try_new(&stream_handle).unwrap();
            let _ai_voice_sink = Arc::new(ai_voice_sink);

            let (ai_audio_playing_tx, ai_audio_playing_rx): (
                flume::Sender<(NamedTempFile, String)>,
                flume::Receiver<(NamedTempFile, String)>,
            ) = flume::bounded(AI_VOICE_SINK_BUFFER_SIZE);

            let (futures_ordered_kill_tx, futures_ordered_kill_rx): (
                flume::Sender<()>,
                flume::Receiver<()>,
            ) = flume::unbounded();

            // Create text to speech conversion thread
            // that will convert text to speech and pass the audio file path to
            // the ai voice audio playing thread
            let thread_ai_tts_rx = ai_tts_rx.clone();
            let thread_voice = voice.clone();
            tokio::spawn(async move {
                // Create the futures ordered queue Used to turn text into speech
                // let (mut converting_tx, mut converting_rx) = tokio::sync::mpsc::unbounded_channel();
                let (converting_tx, converting_rx) = flume::bounded(AI_VOICE_SINK_BUFFER_SIZE);

                {
                    let converting_tx = converting_tx.clone();
                    tokio::spawn(async move {
                        // Queue up any text segments to be turned into speech.
                        while let Ok(ai_text) = thread_ai_tts_rx.recv_async().await {
                            let thread_voice = thread_voice.clone();
                            let thread_ai_text = ai_text.clone();
                            converting_tx
                                .send_async(tokio::spawn(async move {
                                    turn_text_to_speech(thread_ai_text, speech_speed, thread_voice)
                                }))
                                .await
                                .unwrap();

                            debug!(
                                "Sent text-to-speech conversion request to the text-to-speech conversion thread with text: \"{}\"", truncate(&ai_text, 20)
                            );
                        }
                    });
                }

                loop {
                    // tokio sleep is needed here because otherwise this green thread
                    // takes up so much compute that other green threads never get to run.
                    tokio::time::sleep(Duration::from_millis(100)).await;

                    // Empty the futures ordered queue if the kill channel has received a message
                    for _ in futures_ordered_kill_rx.try_iter() {
                        while let Ok(handle) = converting_rx.try_recv() {
                            handle.abort();
                        }
                    }

                    while let Ok(handle) = converting_rx.try_recv() {
                        let handle = handle.await.unwrap();

                        let tempfile_option = handle.await;

                        match tempfile_option {
                            Some((tempfile, ai_text)) => {
                                let mut kill_signal_sent = false;
                                // Empty the futures ordered queue if the kill channel has received a message
                                for _ in futures_ordered_kill_rx.try_iter() {
                                    while let Ok(handle) = converting_rx.try_recv() {
                                        handle.abort();
                                    }
                                    kill_signal_sent = true;
                                }

                                if !kill_signal_sent {
                                    // send tempfile to ai voice audio playing thread
                                    ai_audio_playing_tx.send((tempfile, ai_text)).unwrap();
                                }
                            }
                            None => {
                                // play_audio(&failed_temp_file.path());
                                println_error("failed to turn text to speech");
                            }
                        }
                    }
                }
            });

            // Create the ai voice audio playing thread
            // let thread_ai_voice_sink = ai_voice_sink.clone();
            let thread_ai_audio_playing_rx = ai_audio_playing_rx.clone();
            thread::spawn(move || {
                let runtime = tokio::runtime::Runtime::new()
                    .context("Failed to create tokio runtime")
                    .unwrap();

                let (mut _stream, _stream_handle) = rodio::OutputStream::try_default().unwrap();
                let ai_voice_sink = rodio::Sink::try_new(&stream_handle).unwrap();
                let mut ai_voice_sink = Arc::new(ai_voice_sink);

                for (ai_speech_segment, ai_text) in thread_ai_audio_playing_rx.iter() {
                    // create new stream and sink
                    let (new_stream, stream_handle) = rodio::OutputStream::try_default().unwrap();
                    let new_ai_voice_sink = rodio::Sink::try_new(&stream_handle).unwrap();

                    // put them in the persistent vars
                    ai_voice_sink = Arc::new(new_ai_voice_sink);
                    _stream = new_stream;

                    // play the sound of AI speech
                    let file = std::fs::File::open(ai_speech_segment.path()).unwrap();
                    ai_voice_sink.stop();
                    ai_voice_sink.append(rodio::Decoder::new(BufReader::new(file)).unwrap());
                    info!("Playing AI voice audio: \"{}\"", truncate(&ai_text, 20));

                    // sink.play();

                    while stop_speech_rx.try_recv().is_ok() {}

                    // ai_voice_sink.stop();
                    runtime.block_on(async {
                        let blocking_task = {
                            let ai_voice_sink = ai_voice_sink.clone();

                            task::spawn_blocking(move || {
                                // Your blocking operation here
                                ai_voice_sink.sleep_until_end()
                            })
                        };

                        select! {
                            _ = blocking_task.fuse() => {},
                            _ = stop_speech_rx.recv_async() => {
                                // empty the stop_speech_rx channel.
                                while stop_speech_rx.try_recv().is_ok(){}

                                ai_voice_sink.stop();
                            }
                        };
                    });
                }
            });

            (
                SpeakStream {
                    sentence_accumulator: SentenceAccumulator::new(),
                    ai_tts_tx,
                    ai_tts_rx,
                    futures_ordered_kill_tx,
                    stop_speech_tx,
                    ai_audio_playing_rx,
                },
                _stream,
            )
        }

        pub fn add_token(&mut self, token: &str) {
            // Add the token to the sentence accumulator
            let sentences = self.sentence_accumulator.add_token(token);
            for sentence in sentences {
                self.ai_tts_tx.send(sentence).unwrap();
            }
        }

        pub fn complete_sentence(&mut self) {
            // Process the last sentence
            if let Some(sentence) = self.sentence_accumulator.complete_sentence() {
                self.ai_tts_tx.send(sentence).unwrap();
            }
        }

        pub fn stop_speech(&mut self) {
            // clear all speech channels, stop async executors, and stop the audio sink

            // clear the sentence accumulator
            self.sentence_accumulator.clear_buffer();

            // empty channel of all text messages queued up to be turned into audio speech
            for _ in self.ai_tts_rx.try_iter() {}

            // empty the futures currently turning text to sound
            self.futures_ordered_kill_tx.send(()).unwrap();

            // clear the channel that passes audio files to the ai voice audio playing thread
            for _ in self.ai_audio_playing_rx.try_iter() {}

            // stop the AI voice from speaking the current sentence
            self.stop_speech_tx.send(()).unwrap();
        }
    }
}
