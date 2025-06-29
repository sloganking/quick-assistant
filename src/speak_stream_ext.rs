pub trait SpeakStreamExt {
    fn start_audio_ducking(&self);
    fn stop_audio_ducking(&self);
}

impl SpeakStreamExt for speakstream::ss::SpeakStream {
    fn start_audio_ducking(&self) {}
    fn stop_audio_ducking(&self) {}
}
