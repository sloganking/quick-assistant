use anyhow::Context;
use chrono::{DateTime, Local};
use csv::Reader;
use flume;
use rodio;
use std::{
    io::BufReader,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, LazyLock, RwLock,
    }, // Use LazyLock from std
    thread,
};
use tracing::{info, warn};

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
        wtr.write_record(&["id", "description", "timestamp"])?;
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
    wtr.write_record(&["id", "description", "timestamp"])?;
    for (id, description, timestamp) in timers.iter() {
        wtr.write_record(&[&id.to_string(), description, &timestamp.to_rfc3339()])?;
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
    {
        let mut timers = TIMERS.write().unwrap();
        let original_count = timers.len();
        timers.retain(|(t_id, _, _)| *t_id != id);

        if timers.len() == original_count {
            // No timer with that ID was found
            warn!("No timer found with ID: {}", id);
        }
    }
    // Save to disk after modification
    save_timers_to_disk(&CACHE_DIR.join("timers.csv"))?;
    Ok(())
}

pub fn check_timers() -> Result<Vec<(u64, String, DateTime<Local>)>, anyhow::Error> {
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

pub struct AudibleTimers {
    audio_stop_tx: flume::Sender<()>,
}

impl AudibleTimers {
    pub fn new(audio_file: PathBuf) -> Result<Self, anyhow::Error> {
        let (audio_stop_tx, audio_stop_rx) = flume::unbounded();

        thread::spawn(move || {
            let (_stream, stream_handle) = rodio::OutputStream::try_default().unwrap();
            let sink = rodio::Sink::try_new(&stream_handle).unwrap();

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
                    // You could handle the expired timers here (log them, pass them to your AI, etc.)
                    for (id, description, timestamp) in &expired_timers {
                        info!(
                            "Timer expired (ID: {}): description: \"{}\", time: {}",
                            id,
                            description,
                            &timestamp.to_rfc3339()
                        );
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

        Ok(AudibleTimers { audio_stop_tx })
    }

    pub fn stop_alarm(&self) {
        let _ = self.audio_stop_tx.send(());
    }
}
