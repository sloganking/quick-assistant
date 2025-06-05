use std::sync::{Arc, Mutex};
use cpal::traits::{DeviceTrait, HostTrait};
use rodio::{OutputStream, Sink, Source};

/// Returns the name of the current default output device, if any.
fn default_device_name() -> Option<String> {
    cpal::default_host()
        .default_output_device()
        .and_then(|d| d.name().ok())
}

struct Inner {
    _stream: OutputStream,
    sink: Sink,
    device_name: Option<String>,
}

/// `DefaultDeviceSink` wraps a `rodio::Sink` and recreates the underlying
/// stream and sink if the system default output device changes.
#[derive(Clone)]
pub struct DefaultDeviceSink {
    inner: Arc<Mutex<Inner>>,
}

impl DefaultDeviceSink {
    /// Creates a new `DefaultDeviceSink` using the system default output device.
    pub fn new() -> Self {
        let (stream, handle) = OutputStream::try_default()
            .expect("Failed to open default output stream");
        let sink = Sink::try_new(&handle).expect("Failed to create Sink");
        let name = default_device_name();
        DefaultDeviceSink {
            inner: Arc::new(Mutex::new(Inner {
                _stream: stream,
                sink,
                device_name: name,
            })),
        }
    }

    /// Checks if the default output device has changed. If so, recreates the
    /// stream and sink so future sounds play on the new device.
    fn ensure_device(inner: &mut Inner) {
        let current = default_device_name();
        if current != inner.device_name {
            let (stream, handle) = OutputStream::try_default()
                .expect("Failed to open default output stream");
            let sink = Sink::try_new(&handle).expect("Failed to create Sink");
            // Stop old sink so it doesn't continue playing.
            inner.sink.stop();
            inner._stream = stream;
            inner.sink = sink;
            inner.device_name = current;
        }
    }

    /// Appends a source to the sink, ensuring that the default device is current.
    pub fn append<S>(&self, source: S)
    where
        S: Source + Send + 'static,
        S::Item: rodio::Sample + Send,
        f32: cpal::FromSample<S::Item>,
    {
        let mut inner = self.inner.lock().unwrap();
        Self::ensure_device(&mut inner);
        inner.sink.append(source);
    }

    /// Stops playback and clears queued sounds.
    pub fn stop(&self) {
        let inner = self.inner.lock().unwrap();
        inner.sink.stop();
    }

    pub fn play(&self) {
        let inner = self.inner.lock().unwrap();
        inner.sink.play();
    }

    pub fn pause(&self) {
        let inner = self.inner.lock().unwrap();
        inner.sink.pause();
    }

    pub fn is_paused(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.sink.is_paused()
    }

    pub fn clear(&self) {
        let inner = self.inner.lock().unwrap();
        inner.sink.clear();
    }

    pub fn skip_one(&self) {
        let inner = self.inner.lock().unwrap();
        inner.sink.skip_one();
    }

    pub fn sleep_until_end(&self) {
        let inner = self.inner.lock().unwrap();
        inner.sink.sleep_until_end();
    }

    pub fn empty(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.sink.empty()
    }

    pub fn len(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.sink.len()
    }

    pub fn volume(&self) -> f32 {
        let inner = self.inner.lock().unwrap();
        inner.sink.volume()
    }

    pub fn set_volume(&self, value: f32) {
        let inner = self.inner.lock().unwrap();
        inner.sink.set_volume(value);
    }

    pub fn speed(&self) -> f32 {
        let inner = self.inner.lock().unwrap();
        inner.sink.speed()
    }

    pub fn set_speed(&self, value: f32) {
        let inner = self.inner.lock().unwrap();
        inner.sink.set_speed(value);
    }
}

