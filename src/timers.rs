use anyhow::Context;
use chrono::{DateTime, Local, Utc};
use csv::Reader;
use std::{
    io::BufReader,
    path::{Path, PathBuf},
    sync::LazyLock,
    thread,
};
use tracing::warn;

use crate::CACHE_DIR;

// const CSV_FILE: PathBuf = (*CACHE_DIR).join("timers.csv");

static CSV_FILE: LazyLock<PathBuf> = LazyLock::new(|| CACHE_DIR.join("timers.csv"));

pub fn get_timers() -> Result<Vec<(String, DateTime<Local>)>, anyhow::Error> {
    // make sure csv file exists
    if !CSV_FILE.is_file() {
        let mut wtr = csv::Writer::from_path(&*CSV_FILE)?;
        wtr.write_record(&["name", "timestamp"])?;
        wtr.flush()?;
    }

    // Open the CSV file
    let mut rdr = Reader::from_path(&*CSV_FILE)?;

    // Read records
    let mut records = Vec::new();
    for result in rdr.records() {
        let record = result?;
        let name = &record[0];
        let timestamp: DateTime<Local> = record[1].parse()?; // Parse ISO 8601 UTC timestamp
        records.push((name.to_string(), timestamp));
    }

    Ok(records)
}

fn save_timers(timers: Vec<(String, DateTime<Local>)>) -> Result<(), anyhow::Error> {
    // Write the timers to the CSV file
    let mut wtr = csv::Writer::from_path(&*CSV_FILE)?;

    // Write the header
    wtr.write_record(&["name", "timestamp"])?;

    // Write each timer
    for (name, timestamp) in timers {
        // Convert the UTC timestamp to local time
        let local_timestamp = timestamp.with_timezone(&Local);
        wtr.write_record(&[name, local_timestamp.to_rfc3339()])?;
        // wtr.write_record(&[name, timestamp.to_rfc3339()])?;
    }

    // Finish writing
    wtr.flush()?;

    Ok(())
}

/// Sets a new timer at the given time
pub fn set_timer(timer_time: DateTime<Local>) -> Result<(), anyhow::Error> {
    let mut timers = get_timers()?;

    // Add the new timer
    timers.push(("New Timer".to_string(), timer_time));

    // Save the timers
    save_timers(timers)?;

    Ok(())
}

pub fn check_timers() -> Result<Vec<(String, DateTime<Local>)>, anyhow::Error> {
    // Remove expired timers
    let mut expired_timers: Vec<(String, DateTime<Local>)> = Vec::new();
    let mut timers = get_timers()?;
    timers.retain(|(name, timestamp)| {
        if timestamp <= &Utc::now() {
            // keep track of expired timers
            expired_timers.push((name.clone(), *timestamp));
            false
        } else {
            true
        }
    });

    // Save the still valid timers
    save_timers(timers)?;

    Ok(expired_timers)
}

pub struct AudibleTimers {
    audio_stop_tx: flume::Sender<()>,
}

impl AudibleTimers {
    pub fn new(audio_file: PathBuf) -> Result<Self, anyhow::Error> {
        // create channels
        let (audio_stop_tx, audio_stop_rx) = flume::bounded(1);

        // Create the audio playing channel
        thread::spawn(move || {
            let (_stream, stream_handle) = rodio::OutputStream::try_default().unwrap();
            let sink = rodio::Sink::try_new(&stream_handle).unwrap();

            let mut timer_error_was_logged = false;

            loop {
                // clear the timer clear channel
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
                    loop {
                        // Play the audio file
                        let file = std::fs::File::open(&audio_file).unwrap();
                        sink.stop();
                        sink.append(rodio::Decoder::new(BufReader::new(file)).unwrap());
                        sink.sleep_until_end();

                        if audio_stop_rx.try_recv().is_ok() {
                            break;
                        }
                    }
                }

                // sleep for a bit
                thread::sleep(std::time::Duration::from_secs(1));
            }
        });

        Ok(AudibleTimers { audio_stop_tx })
    }

    pub fn stop_alarm(&self) {
        self.audio_stop_tx.send(()).unwrap();
    }
}
