use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

/// Convert `input` to `output_ext` using ffmpeg and return the path
/// to the converted file.
///
/// The resulting file is created in the system temp directory with a
/// random name so the original file is never overwritten.
pub fn convert_with_ffmpeg(input: &Path, output_ext: &str) -> Result<PathBuf> {
    let out_path = std::env::temp_dir().join(format!("converted-{}.{output_ext}", Uuid::new_v4()));

    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-i",
            input
                .to_str()
                .context("Failed to convert input path to string")?,
            out_path
                .to_str()
                .context("Failed to convert output path to string")?,
        ])
        .status()
        .context("Failed to execute ffmpeg")?;

    if !status.success() {
        bail!("ffmpeg failed to convert file");
    }

    Ok(out_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

    #[test]
    fn convert_with_ffmpeg_fails_if_ffmpeg_returns_error() {
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

        let res = convert_with_ffmpeg(&input, "mp3");
        assert!(res.is_err());

        env::set_var("PATH", old_path);
    }
}
