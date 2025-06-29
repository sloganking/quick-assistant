use cpal::traits::{DeviceTrait, HostTrait};
use once_cell::sync::Lazy;
use rodio::{OutputStream, Sink, Source};
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

/// Globally selected output device name. `None` means use the system default.
pub static SELECTED_OUTPUT_DEVICE: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

struct AudioBuffer {
    channels: u16,
    sample_rate: u32,
    data: Arc<Vec<f32>>,
    pos: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct ResumableSource {
    channels: u16,
    sample_rate: u32,
    data: Arc<Vec<f32>>,
    pos: Arc<AtomicUsize>,
}

impl Iterator for ResumableSource {
    type Item = f32;
    fn next(&mut self) -> Option<Self::Item> {
        let idx = self.pos.fetch_add(1, Ordering::Relaxed);
        self.data.get(idx).copied()
    }
}

impl Source for ResumableSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn total_duration(&self) -> Option<std::time::Duration> {
        let len = self.data.len() as f32 / self.channels as f32 / self.sample_rate as f32;
        Some(std::time::Duration::from_secs_f32(len))
    }
}

/// Returns the name of the current default output device, if any.
pub fn default_device_name() -> Option<String> {
    cpal::default_host()
        .default_output_device()
        .and_then(|d| d.name().ok())
}

/// Returns a list of available output device names.
pub fn list_output_devices() -> Vec<String> {
    let host = cpal::default_host();
    match host.output_devices() {
        Ok(devices) => devices
            .filter_map(|d| d.name().ok())
            .collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    }
}

/// Set the globally selected output device. Pass `None` to use the system default.
pub fn set_output_device(device: Option<String>) {
    *SELECTED_OUTPUT_DEVICE.lock().unwrap() = device;
}

/// Returns the currently selected output device. `None` means the system default.
pub fn get_output_device() -> Option<String> {
    SELECTED_OUTPUT_DEVICE.lock().unwrap().clone()
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
    fn create_stream() -> (OutputStream, Sink, Option<String>) {
        if let Some(selected) = get_output_device() {
            let host = cpal::default_host();
            if let Ok(devices) = host.output_devices() {
                for device in devices {
                    if let Ok(device_name) = device.name() {
                        if device_name == selected {
                            if let Ok((stream, handle)) =
                                OutputStream::try_from_device(&device)
                            {
                                let sink = Sink::try_new(&handle)
                                    .expect("Failed to create Sink");
                                return (stream, sink, Some(device_name));
                            }
                        }
                    }
                }
            }
        }

        let (stream, handle) =
            OutputStream::try_default().expect("Failed to open default output stream");
        let sink = Sink::try_new(&handle).expect("Failed to create Sink");
        let name = default_device_name();
        (stream, sink, name)
    }

    /// Creates a new `DefaultDeviceSink` using the system default output device.
    pub fn new() -> Self {
        let (stream, sink, name) = Self::create_stream();
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
    fn sync_queue(inner: &mut Inner) {
        while inner.queue.len() > inner.sink.len() {
            inner.queue.pop_front();
        }
    }

    fn ensure_device(inner: &mut Inner) {
        Self::sync_queue(inner);

        let desired = match get_output_device() {
            Some(name) => Some(name),
            None => default_device_name(),
        };

        if desired != inner.device_name {
            let volume = inner.sink.volume();
            let speed = inner.sink.speed();
            let paused = inner.sink.is_paused();

            let (stream, new_sink, name) = Self::create_stream();
            new_sink.set_volume(volume);
            new_sink.set_speed(speed);

            // Restart queued buffers on the new sink at their current positions
            for buf in &inner.queue {
                let source = ResumableSource {
                    channels: buf.channels,
                    sample_rate: buf.sample_rate,
                    data: buf.data.clone(),
                    pos: buf.pos.clone(),
                };
                new_sink.append(source);
            }

            if paused {
                new_sink.pause();
            }

            inner.sink.stop();
            inner._stream = stream;
            inner.sink = new_sink;
            inner.device_name = name;
        }
    }

    /// Appends a source to the sink, ensuring that the default device is current.
    pub fn append<T>(&self, input: T)
    where
        T: Source + Send + 'static,
        T::Item: rodio::Sample + Send,
        f32: cpal::FromSample<T::Item>,
    {
        let mut inner = self.inner.lock().unwrap();
        Self::ensure_device(&mut inner);
        let channels = input.channels();
        let sample_rate = input.sample_rate();
        let samples: Vec<f32> = input.convert_samples().collect();
        let arc = Arc::new(samples);
        let pos = Arc::new(AtomicUsize::new(0));
        let buf = AudioBuffer {
            channels,
            sample_rate,
            data: arc.clone(),
            pos: pos.clone(),
        };
        let source = ResumableSource {
            channels,
            sample_rate,
            data: arc,
            pos,
        };
        inner.queue.push_back(buf);
        inner.sink.append::<ResumableSource>(source);
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
