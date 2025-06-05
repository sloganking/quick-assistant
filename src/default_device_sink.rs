use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use cpal::traits::{DeviceTrait, HostTrait};
use rodio::{buffer::SamplesBuffer, OutputStream, Sink, Source};

struct AudioBuffer {
    channels: u16,
    sample_rate: u32,
    data: Arc<Vec<f32>>,
}

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
    queue: VecDeque<AudioBuffer>,
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
                queue: VecDeque::new(),
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
            let mut new_sink = Sink::try_new(&handle).expect("Failed to create Sink");
            // Restart queued buffers on the new sink
            for buf in &inner.queue {
                let buffer = SamplesBuffer::new(buf.channels, buf.sample_rate, (*buf.data).clone());
                new_sink.append::<SamplesBuffer<f32>>(buffer);
            }
            inner.sink.stop();
            inner._stream = stream;
            inner.sink = new_sink;
            inner.device_name = current;
        }
    }

    /// Appends a source to the sink, ensuring that the default device is current.
    pub fn append<T>(&self, source: T)
    where
        T: Source + Send + 'static,
        T::Item: rodio::Sample + Send,
        f32: cpal::FromSample<T::Item>,
    {
        let mut inner = self.inner.lock().unwrap();
        Self::ensure_device(&mut inner);
        let channels = source.channels();
        let sample_rate = source.sample_rate();
        let samples: Vec<f32> = source.convert_samples().collect();
        let arc = Arc::new(samples);
        let buffer = SamplesBuffer::new(channels, sample_rate, (*arc).clone());
        inner.queue.push_back(AudioBuffer {
            channels,
            sample_rate,
            data: arc.clone(),
        });
        inner.sink.append::<SamplesBuffer<f32>>(buffer);
    }

    /// Stops playback and clears queued sounds.
    pub fn stop(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.sink.stop();
        inner.queue.clear();
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
        let mut inner = self.inner.lock().unwrap();
        inner.sink.clear();
        inner.queue.clear();
    }

    pub fn skip_one(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.sink.skip_one();
        if !inner.queue.is_empty() {
            inner.queue.pop_front();
        }
    }

    pub fn sleep_until_end(&self) {
        let mut inner = self.inner.lock().unwrap();
        Self::ensure_device(&mut inner);
        inner.sink.sleep_until_end();
    }

    pub fn empty(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        Self::ensure_device(&mut inner);
        inner.sink.empty()
    }

    pub fn len(&self) -> usize {
        let mut inner = self.inner.lock().unwrap();
        Self::ensure_device(&mut inner);
        inner.sink.len()
    }

    pub fn volume(&self) -> f32 {
        let mut inner = self.inner.lock().unwrap();
        Self::ensure_device(&mut inner);
        inner.sink.volume()
    }

    pub fn set_volume(&self, value: f32) {
        let mut inner = self.inner.lock().unwrap();
        Self::ensure_device(&mut inner);
        inner.sink.set_volume(value);
    }

    pub fn speed(&self) -> f32 {
        let mut inner = self.inner.lock().unwrap();
        Self::ensure_device(&mut inner);
        inner.sink.speed()
    }

    pub fn set_speed(&self, value: f32) {
        let mut inner = self.inner.lock().unwrap();
        Self::ensure_device(&mut inner);
        inner.sink.set_speed(value);
    }
}

