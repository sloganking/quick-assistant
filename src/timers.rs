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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use chrono::{Local, Duration};
    use std::env;

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
}
