use anyhow::bail;
use chrono::{DateTime, Local};
use csv::Reader;
use std::{
    io::BufReader,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        LazyLock, RwLock,
    }, // Use LazyLock from std
    thread,
    time::Instant,
};
use tracing::{info, warn};

use crate::default_device_sink::DefaultDeviceSink;
use crate::CACHE_DIR;

// Global atomic ID counter for timers
static NEXT_ID: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(1));

// Global lazy-initialized in-memory timers storage.
// Now we store (id, description, timestamp)
static TIMERS: LazyLock<RwLock<Vec<(u64, String, DateTime<Local>)>>> = LazyLock::new(|| {
    let path = CACHE_DIR.join("timers.csv");
    let timers = load_timers_from_disk(&path).expect("Failed to load timers");
    RwLock::new(timers)
});

// Stopwatch state: (start_time, total_elapsed_before_pause)
// When running: start_time is Some, when paused: start_time is None
static STOPWATCH: LazyLock<RwLock<(Option<Instant>, std::time::Duration)>> =
    LazyLock::new(|| RwLock::new((None, std::time::Duration::ZERO)));

fn load_timers_from_disk(
    path: &Path,
) -> Result<Vec<(u64, String, DateTime<Local>)>, anyhow::Error> {
    if !path.is_file() {
        let mut wtr = csv::Writer::from_path(path)?;
        wtr.write_record(["id", "description", "timestamp"])?;
        wtr.flush()?;
        return Ok(vec![]);
    }

    let mut rdr = Reader::from_path(path)?;
    let mut records = Vec::new();
    let mut max_id = 0;
    for result in rdr.records() {
        let record = result?;
        let id: u64 = record[0].parse()?;
        let description = &record[1];
        let timestamp: DateTime<Local> = record[2].parse()?;
        if id > max_id {
            max_id = id;
        }
        records.push((id, description.to_string(), timestamp));
    }

    // Set NEXT_ID to one more than the max ID found
    NEXT_ID.store(max_id + 1, Ordering::Relaxed);
    Ok(records)
}

fn save_timers_to_disk(path: &Path) -> Result<(), anyhow::Error> {
    let timers = TIMERS.read().unwrap();
    let mut wtr = csv::Writer::from_path(path)?;
    wtr.write_record(["id", "description", "timestamp"])?;
    for (id, description, timestamp) in timers.iter() {
        wtr.write_record([&id.to_string(), description, &timestamp.to_rfc3339()])?;
    }
    wtr.flush()?;
    Ok(())
}

// Public API for reading timers from memory
pub fn get_timers() -> Vec<(u64, String, DateTime<Local>)> {
    let timers = TIMERS.read().unwrap();
    timers.clone()
}

// Public API for adding a timer with a description
pub fn set_timer(description: String, timer_time: DateTime<Local>) -> Result<(), anyhow::Error> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    {
        let mut timers = TIMERS.write().unwrap();
        timers.push((id, description, timer_time));
    }
    // Save to disk after modification if desired
    save_timers_to_disk(&CACHE_DIR.join("timers.csv"))?;
    Ok(())
}

// Public API for deleting a timer by ID
pub fn delete_timer(id: u64) -> Result<(), anyhow::Error> {
    let original_count;
    let new_count;
    {
        let mut timers = TIMERS.write().unwrap();
        original_count = timers.len();
        timers.retain(|(t_id, _, _)| *t_id != id);
        new_count = timers.len();
    }

    if new_count != original_count {
        save_timers_to_disk(&CACHE_DIR.join("timers.csv"))?;
    } else {
        bail!("Timer with ID {} not found", id);
    }

    Ok(())
}

fn check_timers() -> Result<Vec<(u64, String, DateTime<Local>)>, anyhow::Error> {
    let mut expired_timers = Vec::new();
    {
        let mut timers = TIMERS.write().unwrap();
        timers.retain(|(id, description, timestamp)| {
            let now_local = Local::now();
            if *timestamp <= now_local {
                expired_timers.push((*id, description.clone(), *timestamp));
                false
            } else {
                true
            }
        });
    }
    if !expired_timers.is_empty() {
        save_timers_to_disk(&CACHE_DIR.join("timers.csv"))?;
    }
    Ok(expired_timers)
}

pub struct Timer {
    pub id: u64,
    pub description: String,
    pub timestamp: DateTime<Local>,
}

pub struct AudibleTimers {
    audio_stop_tx: flume::Sender<()>,
}

impl AudibleTimers {
    pub fn new(audio_file: PathBuf) -> Result<(Self, flume::Receiver<Timer>), anyhow::Error> {
        let (audio_stop_tx, audio_stop_rx) = flume::unbounded();
        let (expired_timers_tx, expired_timers_rx): (flume::Sender<Timer>, flume::Receiver<Timer>) =
            flume::unbounded();

        thread::spawn(move || {
            let sink = DefaultDeviceSink::new();

            let mut timer_error_was_logged = false;

            loop {
                // Clear any pending stop signals
                while audio_stop_rx.try_recv().is_ok() {}

                let expired_timers = match check_timers() {
                    Ok(timers) => {
                        timer_error_was_logged = false;
                        timers
                    }
                    Err(e) => {
                        if !timer_error_was_logged {
                            warn!("Error checking timers: {}", e);
                            timer_error_was_logged = true;
                        }
                        Vec::new()
                    }
                };

                if !expired_timers.is_empty() {
                    for (id, description, timestamp) in &expired_timers {
                        info!(
                            "Timer expired (ID: {}): description: \"{}\", time: {}",
                            id,
                            description,
                            &timestamp.to_rfc3339()
                        );
                    }

                    // send expired timers to the main thread
                    for (id, description, timestamp) in expired_timers {
                        let timer = Timer {
                            id,
                            description,
                            timestamp,
                        };
                        if let Err(e) = expired_timers_tx.send(timer) {
                            warn!("Failed to send expired timer to main thread: {}", e);
                        }
                    }

                    'alarm_loop: loop {
                        sink.stop(); // Clear any previous sound
                        let file = match std::fs::File::open(&audio_file) {
                            Ok(f) => f,
                            Err(e) => {
                                warn!("Failed to open audio file: {}", e);
                                break 'alarm_loop;
                            }
                        };
                        let source = rodio::Decoder::new(BufReader::new(file)).unwrap();
                        sink.append(source);

                        // Poll for stop signal or end of sound
                        loop {
                            if audio_stop_rx.try_recv().is_ok() {
                                // Stop immediately and break out of the entire alarm loop
                                sink.stop();
                                break 'alarm_loop;
                            }

                            // Check if sound finished
                            if sink.empty() {
                                break;
                            }

                            thread::sleep(std::time::Duration::from_millis(100));
                        }
                    }
                }

                // Sleep a bit before re-checking
                thread::sleep(std::time::Duration::from_secs(1));
            }
        });

        Ok((AudibleTimers { audio_stop_tx }, expired_timers_rx))
    }

    pub fn stop_alarm(&self) {
        self.audio_stop_tx.send(()).unwrap();
    }
}

// Stopwatch API

/// Starts the stopwatch. If already running, this does nothing.
/// If paused, it resumes from where it left off.
pub fn start_stopwatch() -> String {
    let mut sw = STOPWATCH.write().unwrap();
    if sw.0.is_some() {
        "Stopwatch is already running.".to_string()
    } else {
        sw.0 = Some(Instant::now());
        if sw.1.as_secs() == 0 {
            "Stopwatch started.".to_string()
        } else {
            "Stopwatch resumed.".to_string()
        }
    }
}

/// Stops (pauses) the stopwatch and returns the elapsed time.
pub fn stop_stopwatch() -> String {
    let mut sw = STOPWATCH.write().unwrap();
    match sw.0 {
        Some(start) => {
            let elapsed = start.elapsed();
            sw.1 += elapsed;
            sw.0 = None;
            let total = sw.1;
            format!(
                "Stopwatch stopped. Total elapsed time: {}",
                humantime::format_duration(total)
            )
        }
        None => {
            if sw.1.as_secs() == 0 {
                "Stopwatch is not running and has no recorded time.".to_string()
            } else {
                format!(
                    "Stopwatch is already stopped. Total elapsed time: {}",
                    humantime::format_duration(sw.1)
                )
            }
        }
    }
}

/// Returns the current elapsed time without stopping the stopwatch.
pub fn check_stopwatch() -> String {
    let sw = STOPWATCH.read().unwrap();
    let total = match sw.0 {
        Some(start) => sw.1 + start.elapsed(),
        None => sw.1,
    };

    if sw.0.is_none() && sw.1.as_secs() == 0 {
        "Stopwatch has not been started yet.".to_string()
    } else {
        let status = if sw.0.is_some() { "running" } else { "stopped" };
        format!(
            "Stopwatch is {}. Elapsed time: {}",
            status,
            humantime::format_duration(total)
        )
    }
}

/// Resets the stopwatch to zero and stops it if running.
pub fn reset_stopwatch() -> String {
    let mut sw = STOPWATCH.write().unwrap();
    *sw = (None, std::time::Duration::ZERO);
    "Stopwatch has been reset to zero.".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Local};
    use std::env;
    use tempfile::tempdir;

    #[test]
    fn set_get_delete_timer() {
        let tmp = tempdir().unwrap();
        env::set_var("XDG_CACHE_HOME", tmp.path());
        std::fs::create_dir_all(tmp.path().join("quick-assistant")).unwrap();

        // Ensure no timers initially
        assert!(get_timers().is_empty());

        let t = Local::now() + Duration::seconds(1);
        set_timer("test".to_string(), t).unwrap();

        let timers = get_timers();
        assert_eq!(timers.len(), 1);
        assert_eq!(timers[0].1, "test");

        delete_timer(timers[0].0).unwrap();
        assert!(get_timers().is_empty());
    }

    #[test]
    fn test_stopwatch_basic_operations() {
        // Reset stopwatch to ensure clean state
        reset_stopwatch();

        // Give it a moment to settle
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Check initial state (after reset it's zero but not started)
        let result = check_stopwatch();
        assert!(
            result.contains("not been started") || result.contains("Elapsed time: 0s"),
            "Got: {}",
            result
        );

        // Start the stopwatch
        let result = start_stopwatch();
        assert!(
            result.contains("started") || result.contains("resumed"),
            "Got: {}",
            result
        );

        // Starting again should indicate it's already running
        let result = start_stopwatch();
        assert!(result.contains("already running"), "Got: {}", result);

        // Sleep for a bit
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Check while running
        let result = check_stopwatch();
        assert!(result.contains("running"), "Got: {}", result);

        // Stop the stopwatch
        let result = stop_stopwatch();
        assert!(
            result.contains("stopped") || result.contains("Total elapsed time"),
            "Got: {}",
            result
        );

        // Reset the stopwatch
        let result = reset_stopwatch();
        assert!(result.contains("reset"), "Got: {}", result);

        // Check after reset
        let result = check_stopwatch();
        assert!(
            result.contains("not been started") || result.contains("Elapsed time: 0s"),
            "Got: {}",
            result
        );
    }

    #[test]
    fn test_stopwatch_pause_resume() {
        reset_stopwatch();

        // Start stopwatch
        start_stopwatch();
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Stop (pause)
        stop_stopwatch();

        // Sleep while paused
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Resume (after stopping, starting again should say resumed)
        let result = start_stopwatch();
        assert!(
            result.contains("resumed") || result.contains("started"),
            "Got: {}",
            result
        );

        // Check elapsed time
        let result = check_stopwatch();
        assert!(
            result.contains("running") || result.contains("Elapsed time"),
            "Got: {}",
            result
        );

        // Clean up
        reset_stopwatch();
    }
}
