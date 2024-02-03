pub mod speakstream {
    use futures::stream::FuturesOrdered;
    use futures::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use tempfile::NamedTempFile;

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

    /// SpeakStream
    pub struct SpeakStream {
        sentence_accumulator: SentenceAccumulator,
        futures_ordered_mutex:
            Arc<Mutex<FuturesOrdered<Pin<Box<dyn Future<Output = Option<NamedTempFile>>>>>>>,
    }

    impl SpeakStream {
        pub fn new() -> Self {
            // The sentence accumulator sends sentences to this channel to be turned into speech audio
            let (ai_tts_tx, ai_tts_rx): (flume::Sender<String>, flume::Receiver<String>) =
                flume::unbounded();

            let futures_ordered_mutex = Arc::new(Mutex::new(FuturesOrdered::new()));

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
                        futures_ordered.push_back(turn_text_to_speech(ai_text, opt.speech_speed));
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
