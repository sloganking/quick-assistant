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

/// Moves audio to mp3.
/// Ignores output's extension if it is passed one.
/// Returns the new path.
#[instrument(skip_all)]
fn move_audio_to_mp3(input: &Path, output: &Path) -> Result<PathBuf, anyhow::Error> {
    let mut output = PathBuf::from(output);
    output.set_extension("mp3");

    debug!("Running ffmpeg to convert audio to mp3.");
    // `ffmpeg -i input.mp4 -q:a 0 -map a output.mp3`
    let cmd_output = match Command::new("ffmpeg")
        .args([
            "-i",
            input
                .to_str()
                .context("Failed to convert input path to string")?,
            "-q:a",
            "0",
            "-map",
            "a",
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

    if !cmd_output.status.success() {
        bail!("ffmpeg failed to convert audio");
    }

    debug!("ffmpeg succeeded converting audio to mp3.");

    Ok(output)
}

#[instrument(skip_all)]
pub async fn transcribe(
    client: &Client<OpenAIConfig>,
    input: &Path,
) -> Result<String, Box<dyn Error>> {
    let tmp_dir = tempdir().context("Failed to create temp dir.")?;
    let tmp_mp3_path = tmp_dir.path().join("tmp.mp3");

    // Make input file an mp3 if it is not
    // We do this to get around the api file size limit:
    // Error: ApiError(ApiError { message: "Maximum content size limit (26214400) exceeded (26228340 bytes read)", type: "server_error", param: None, code: None })
    let input_mp3 = if input.extension().unwrap_or_default() != "mp3" {
        // println!("{:?}", tmp_dir.path());
        debug!("Converting audio to mp3.");
        move_audio_to_mp3(input, &tmp_mp3_path).context("Failed to convert audio to mp3.")?
    } else {
        // println!("{:?}", input);
        debug!("Audio is already mp3.");
        PathBuf::from(input)
    };

    debug!("creating transcription request.");
    let request = CreateTranscriptionRequestArgs::default()
            .file(input_mp3)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

    #[test]
    fn move_audio_to_mp3_fails_if_ffmpeg_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let ffmpeg_path = dir.path().join("ffmpeg");
        fs::write(&ffmpeg_path, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&ffmpeg_path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&ffmpeg_path, perms).unwrap();
        }

        let old_path = env::var("PATH").unwrap_or_default();
        env::set_var("PATH", format!("{}:{}", dir.path().display(), old_path));

        let input = dir.path().join("in.wav");
        fs::write(&input, b"dummy").unwrap();
        let out = dir.path().join("out.mp3");

        let res = move_audio_to_mp3(&input, &out);
        assert!(res.is_err());

        env::set_var("PATH", old_path);
    }
}
