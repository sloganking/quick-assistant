use anyhow::Context;
use chrono::{DateTime, Local, Utc};
use csv::Reader;
use flume;
use rodio;
use std::{
    io::BufReader,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, RwLock}, // Use LazyLock from std
    thread,
};
use tracing::warn;

use crate::CACHE_DIR;

// Global lazy-initialized in-memory timers storage
static TIMERS: LazyLock<RwLock<Vec<(String, DateTime<Local>)>>> = LazyLock::new(|| {
    let path = CACHE_DIR.join("timers.csv");
    let timers = load_timers_from_disk(&path).expect("Failed to load timers");
    RwLock::new(timers)
});

fn load_timers_from_disk(path: &Path) -> Result<Vec<(String, DateTime<Local>)>, anyhow::Error> {
    if !path.is_file() {
        let mut wtr = csv::Writer::from_path(path)?;
        wtr.write_record(&["name", "timestamp"])?;
        wtr.flush()?;
        return Ok(vec![]);
    }

    let mut rdr = Reader::from_path(path)?;
    let mut records = Vec::new();
    for result in rdr.records() {
        let record = result?;
        let name = &record[0];
        let timestamp: DateTime<Local> = record[1].parse()?;
        records.push((name.to_string(), timestamp));
    }

    Ok(records)
}

fn save_timers_to_disk(path: &Path) -> Result<(), anyhow::Error> {
    let timers = TIMERS.read().unwrap();
    let mut wtr = csv::Writer::from_path(path)?;
    wtr.write_record(&["name", "timestamp"])?;
    for (name, timestamp) in timers.iter() {
        wtr.write_record(&[name, &timestamp.to_rfc3339()])?;
    }
    wtr.flush()?;
    Ok(())
}

// Public API for reading timers from memory
pub fn get_timers() -> Vec<(String, DateTime<Local>)> {
    let timers = TIMERS.read().unwrap();
    timers.clone()
}

// Public API for adding a timer
pub fn set_timer(timer_time: DateTime<Local>) -> Result<(), anyhow::Error> {
    {
        let mut timers = TIMERS.write().unwrap();
        timers.push(("New Timer".to_string(), timer_time));
    }
    // Save to disk after modification if desired
    save_timers_to_disk(&CACHE_DIR.join("timers.csv"))?;
    Ok(())
}

pub fn check_timers() -> Result<Vec<(String, DateTime<Local>)>, anyhow::Error> {
    let mut expired_timers = Vec::new();
    {
        let mut timers = TIMERS.write().unwrap();
        timers.retain(|(name, timestamp)| {
            let now_local = Local::now();
            if *timestamp <= now_local {
                expired_timers.push((name.clone(), *timestamp));
                false
            } else {
                true
            }
        });
    }
    save_timers_to_disk(&CACHE_DIR.join("timers.csv"))?;
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
                    // Label the outer alarm loop so we can break out of it directly
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
                                // Sound finished playing naturally. If you want continuous replay until stopped,
                                // just `break` to restart from the outer loop. If you only want to play once per
                                // expired timer batch, `break 'alarm_loop;` instead.
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
