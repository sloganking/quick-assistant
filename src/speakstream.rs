pub mod speakstream {
    use anyhow::Context;
    use async_openai::{
        types::{
            ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
            ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
            CreateChatCompletionRequestArgs, CreateSpeechRequestArgs, SpeechModel, Voice,
        },
        Client,
    };
    use async_std::future;
    use colored::Colorize;
    use futures::stream::FuturesOrdered;
    use futures::Future;
    use rodio::OutputStream;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::{io::BufReader, time::Duration};
    use tempfile::{Builder, NamedTempFile};

    fn println_error(err: &str) {
        println!("{}: {}", "Error".truecolor(255, 0, 0), err);
    }

    /// SentenceAccumulator is a struct that accumulates tokens into sentences
    /// before sending the sentences to the AI voice channel.
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

    /// SpeakStream is a struct that accumulates tokens into sentences
    /// Once a sentence is complete, it speaks the sentence using the AI voice.
    pub struct SpeakStream {
        sentence_accumulator: SentenceAccumulator,
        futures_ordered_mutex:
            Arc<Mutex<FuturesOrdered<Pin<Box<dyn Future<Output = Option<NamedTempFile>>>>>>>,
        // Arc<Mutex<FuturesOrdered<impl Future<Output = Option<NamedTempFile>>>>>,
        ai_voice_sink: Arc<rodio::Sink>,
        stream: OutputStream,
        speech_speed: f32,
    }

    impl SpeakStream {
        pub fn new() -> Self {
            // The sentence accumulator sends sentences to this channel to be turned into speech audio
            let (ai_tts_tx, ai_tts_rx): (flume::Sender<String>, flume::Receiver<String>) =
                flume::unbounded();

            // Create a mutex to hold the futures ordered queue
            // Used to turn text into speech
            let futures_ordered_mutex = Arc::new(Mutex::new(FuturesOrdered::new()));

            // Create the AI voice audio sink
            let (_stream, stream_handle) = rodio::OutputStream::try_default().unwrap();
            let ai_voice_sink = rodio::Sink::try_new(&stream_handle).unwrap();
            let ai_voice_sink = Arc::new(ai_voice_sink);

            let (ai_audio_playing_tx, ai_audio_playing_rx): (
                flume::Sender<NamedTempFile>,
                flume::Receiver<NamedTempFile>,
            ) = flume::unbounded();

            // Create text to speech conversion thread
            // that will convert text to speech and pass the audio file path to
            // the ai voice audio playing thread
            let thread_futures_ordered_mutex = futures_ordered_mutex.clone();
            thread::spawn(move || {
                let runtime = tokio::runtime::Runtime::new()
                    .context("Failed to create tokio runtime")
                    .unwrap();

                loop {
                    let mut futures_ordered = thread_futures_ordered_mutex.lock().unwrap();

                    // Queue up any text segments to be turned into speech.
                    for ai_text in ai_tts_rx.try_iter() {
                        futures_ordered.push_back(turn_text_to_speech(ai_text, 1.0));
                    }

                    // Send any ready audio segments to the ai voice audio playing thread
                    while let Some(tempfile_option) = runtime.block_on(futures_ordered.next()) {
                        match tempfile_option {
                            Some(tempfile) => {
                                // send tempfile to ai voice audio playing thread
                                ai_audio_playing_tx.send(tempfile).unwrap();
                            }
                            None => {
                                // play_audio(&failed_temp_file.path());
                                println_error("failed to turn text to speech");
                            }
                        }
                    }
                    drop(futures_ordered);
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

            SpeakStream {
                sentence_accumulator: SentenceAccumulator::new(ai_tts_tx),
                futures_ordered_mutex,
                ai_voice_sink,
                stream: _stream,
                speech_speed: 1.0,
            }
        }

        fn add_token(&mut self, token: &str) {
            // Add the token to the sentence accumulator
            self.sentence_accumulator.add_token(token);
        }

        fn stop_speech(&mut self) {
            // clear all speech channels, stop async executors, and stop the audio sink

            // // stop the AI voice from speaking
            // {
            //     // stop the LLM
            //     let mut llm_should_stop = thread_llm_should_stop_mutex.lock().unwrap();
            //     *llm_should_stop = true;
            //     drop(llm_should_stop);

            //     // clear the sentence accumulator
            //     let mut thread_sentence_accumulator =
            //         thread_sentence_accumulator_mutex.lock().unwrap();
            //     thread_sentence_accumulator.clear_buffer();
            //     drop(thread_sentence_accumulator);

            //     // empty channel of all text messages queued up to be turned into audio speech
            //     for _ in thread_ai_tts_rx.try_iter() {}

            //     // empty the futures currently turning text to sound
            //     let mut futures_ordered = thread_futures_ordered_mutex.lock().unwrap();
            //     *futures_ordered = FuturesOrdered::new();
            //     drop(futures_ordered);

            //     // clear the channel that passes audio files to the ai voice audio playing thread
            //     for _ in thread_ai_audio_playing_rx.try_iter() {}

            //     // stop the AI voice from speaking the current sentence
            //     thread_ai_voice_sink.stop();
            // }
        }
    }
}
