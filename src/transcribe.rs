use crate::error_and_panic;
use anyhow::{bail, Context};
use async_openai::{config::OpenAIConfig, types::CreateTranscriptionRequestArgs, Client};
use std::{
    error::Error,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::tempdir;
use tracing::{debug, instrument};

/// Converts audio to opus.
/// Ignores output's extension if it is passed one.
/// Returns the new path.
#[instrument(skip_all)]
fn move_audio_to_opus(input: &Path, output: &Path) -> Result<PathBuf, anyhow::Error> {
    let mut output = PathBuf::from(output);
    output.set_extension("opus");

    debug!("Running ffmpeg to convert audio to opus.");
    // `ffmpeg -i input.wav -c:a libopus -b:a 24k -application voip -frame_duration 20 output.opus`
    let _ = match Command::new("ffmpeg")
        .args([
            "-i",
            input
                .to_str()
                .context("Failed to convert input path to string")?,
            "-c:a",
            "libopus",
            "-b:a",
            "24k",
            "-application",
            "voip",
            "-frame_duration",
            "20",
            output
                .to_str()
                .context("Failed to convert output path to string")?,
        ])
        .output()
    {
        Ok(x) => x,
        Err(err) => {
            debug!("ffmpeg failed to convert audio: {:?}", err);
            if err.kind() == std::io::ErrorKind::NotFound {
                error_and_panic("ffmpeg not found. Please install ffmpeg and add it to your PATH");
            } else {
                bail!("ffmpeg failed to convert audio");
            }
        }
    };
    debug!("ffmpeg succeeded converting audio to opus.");

    Ok(output)
}

#[instrument(skip_all)]
pub async fn transcribe(
    client: &Client<OpenAIConfig>,
    input: &Path,
) -> Result<String, Box<dyn Error>> {
    let tmp_dir = tempdir().context("Failed to create temp dir.")?;
    let tmp_opus_path = tmp_dir.path().join("tmp.opus");

    // Make input file an opus if it is not
    // We do this to get around the api file size limit:
    // Error: ApiError(ApiError { message: "Maximum content size limit (26214400) exceeded (26228340 bytes read)", type: "server_error", param: None, code: None })
    let input_opus = if input.extension().unwrap_or_default() != "opus" {
        // println!("{:?}", tmp_dir.path());
        debug!("Converting audio to opus.");
        move_audio_to_opus(input, &tmp_opus_path).context("Failed to convert audio to opus.")?
    } else {
        // println!("{:?}", input);
        debug!("Audio is already opus.");
        PathBuf::from(input)
    };

    debug!("creating transcription request.");
    let request = CreateTranscriptionRequestArgs::default()
            .file(input_opus)
            .model("whisper-1")
            .prompt("And now, a transcription from random language(s) that concludes with perfect punctuation: ")
            .build()
            .context("Failed to build transcription request.")?;

    debug!("sending transcription request.");
    let response = client
        .audio()
        .transcribe(request)
        .await
        .context("Failed to get OpenAI API transcription response.")?;

    Ok(response.text)
}
