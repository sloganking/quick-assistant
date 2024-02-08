pub mod speakstream {

    use anyhow::Context;
    use futures::stream::FuturesOrdered;
    use futures::stream::StreamExt; // For `.next()` on FuturesOrdered.
    use rodio::OutputStream;
    use std::io::BufReader;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;
    use std::thread;
    use tempfile::Builder;
    use tempfile::NamedTempFile;
    // use async_openai::{
    //     types::{
    //         ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
    //         ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
    //         CreateChatCompletionRequestArgs, CreateSpeechRequestArgs, SpeechModel, Voice,
    //     },
    //     Client,
    // };
    use async_openai::{
        types::{CreateSpeechRequestArgs, SpeechModel, Voice},
        Client,
    };
    use async_std::future;
    use colored::Colorize;
    use std::time::Duration;

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
                    // println!();
                    // println!("Bad cut");
                    // println!();
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
                    // println!();
                    // println!("Good cut");
                    // println!();
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
                            // println!();
                            // println!("Perfect cut");
                            // println!();
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

    /// Turns text into speech using the AI voice.
    async fn turn_text_to_speech(
        ai_text: String,
        speed: f32,
        voice: &Voice,
    ) -> Option<NamedTempFile> {
        let client = Client::new();

        // Turn AI's response into speech

        let request = CreateSpeechRequestArgs::default()
            .input(ai_text)
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
                speed,
            );

            return Some(sped_up_audio_path);
        }

        Some(ai_speech_segment_tempfile)
    }

    fn get_second_to_last_char(s: &str) -> Option<char> {
        s.chars().rev().nth(1)
    }

    /// Receives the next future from the futures ordered queue.
    /// Does not return until a future has completed and returned a result.
    /// Similar to channel.recv_async() methods.
    async fn recv_future_ordered(
        futures_ordered: &mut FuturesOrdered<impl futures::Future<Output = Option<NamedTempFile>>>,
    ) -> Option<NamedTempFile> {
        loop {
            let futures_ordered_option = futures_ordered.next().await;

            if let Some(tempfile_option) = futures_ordered_option {
                // This function is technically not cancel-safe, because if the function
                // is canceled at the location of this comment, after `futures_ordered.next()`
                // but before `return tempfile_option;`, the value will be lost.
                return tempfile_option;
            } else {
                // Not sure why but this sleep is necessary to prevent the loop from blocking
                // other futures from being polled in a tokio::select!() macro.
                // My guess is that the loop runs so often that tokio keeps this thread running
                // and doesn't poll other futures.
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    /// SpeakStream is a struct that accumulates tokens into sentences
    /// Once a sentence is complete, it speaks the sentence using the AI voice.
    pub struct SpeakStream {
        sentence_accumulator: SentenceAccumulator,
        ai_tts_tx: flume::Sender<String>,
        ai_tts_rx: flume::Receiver<String>,
        futures_ordered_kill_tx: flume::Sender<()>,
        ai_voice_sink: Arc<rodio::Sink>,
        ai_audio_playing_rx: flume::Receiver<NamedTempFile>,
    }

    impl SpeakStream {
        pub fn new(voice: Voice, speech_speed: f32) -> (Self, OutputStream) {
            // The sentence accumulator sends sentences to this channel to be turned into speech audio
            let (ai_tts_tx, ai_tts_rx): (flume::Sender<String>, flume::Receiver<String>) =
                flume::unbounded();

            // Create the AI voice audio sink
            let (_stream, stream_handle) = rodio::OutputStream::try_default().unwrap();
            let ai_voice_sink = rodio::Sink::try_new(&stream_handle).unwrap();
            let ai_voice_sink = Arc::new(ai_voice_sink);

            let (ai_audio_playing_tx, ai_audio_playing_rx): (
                flume::Sender<NamedTempFile>,
                flume::Receiver<NamedTempFile>,
            ) = flume::unbounded();

            let (futures_ordered_kill_tx, futures_ordered_kill_rx): (
                flume::Sender<()>,
                flume::Receiver<()>,
            ) = flume::unbounded();

            // Create text to speech conversion thread
            // that will convert text to speech and pass the audio file path to
            // the ai voice audio playing thread
            let thread_ai_tts_rx = ai_tts_rx.clone();
            let thread_voice = voice.clone();
            thread::spawn(move || {
                let runtime = tokio::runtime::Runtime::new()
                    .context("Failed to create tokio runtime")
                    .unwrap();

                // Create the futures ordered queue Used to turn text into speech
                let mut futures_ordered = FuturesOrdered::new();

                loop {
                    runtime.block_on(async {
                        tokio::select! {
                            // sleep = tokio::time::sleep(Duration::from_secs(1)) => {
                            //     println!("{} {:?}", "_ = tokio::time::sleep(Duration::from_secs(1)) => ".purple(), sleep);
                            // }

                            // Queue up any text segments to be turned into speech.
                            ai_text_result = thread_ai_tts_rx.recv_async() => {

                                // println!("{}", "thread_ai_tts_rx received... Pushing into futures_ordered".purple());
                                let ai_text = ai_text_result.unwrap();
                                futures_ordered.push_back(turn_text_to_speech(
                                    ai_text,
                                    speech_speed,
                                    &thread_voice,
                                ));
                            }

                            // Handle kill signal
                            result = futures_ordered_kill_rx.recv_async() => {
                                let _ = result.unwrap();

                                // println!("{}", "_ = futures_ordered_kill_rx.recv() =>".purple());
                                futures_ordered = FuturesOrdered::new();

                            }

                            // Send any ready audio segments to the ai voice audio playing thread
                            tempfile_option = recv_future_ordered(&mut futures_ordered) => {
                                // println!("{}","futures_ordered.next()".purple());

                                if let Some(tempfile) = tempfile_option {
                                    // send tempfile to ai voice audio playing thread
                                    // println!("{}", "tempfile being sent to play audio:".purple());
                                    ai_audio_playing_tx.send_async(tempfile).await.unwrap();
                                }
                            }

                        }
                    });
                }
            });

            // Create the ai voice audio playing thread
            let thread_ai_voice_sink = ai_voice_sink.clone();
            let thread_ai_audio_playing_rx = ai_audio_playing_rx.clone();
            tokio::spawn(async move {
                // println!("Waiting for ai_voice_playing_rx");

                for ai_speech_segment in thread_ai_audio_playing_rx.iter() {
                    // play the sound of AI speech
                    let file = std::fs::File::open(ai_speech_segment.path()).unwrap();
                    thread_ai_voice_sink.stop();
                    thread_ai_voice_sink.append(rodio::Decoder::new(BufReader::new(file)).unwrap());
                    // sink.play();

                    thread_ai_voice_sink.sleep_until_end();
                }
            });

            (
                SpeakStream {
                    sentence_accumulator: SentenceAccumulator::new(),
                    ai_tts_tx,
                    ai_tts_rx,
                    futures_ordered_kill_tx,
                    ai_voice_sink,
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
            self.ai_voice_sink.stop();
        }
    }
}
