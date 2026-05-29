use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegPreflight {
    pub ffmpeg_path: PathBuf,
    pub ffprobe_path: PathBuf,
    pub ffmpeg_version: String,
    pub ffprobe_version: String,
    pub hevc_encoder: String,
    pub svtav1_encoder: String,
    pub libaom_encoder: String,
    pub aac_encoder: String,
    pub opus_encoder: String,
    pub matroska_muxer: String,
    pub mp4_muxer: String,
    pub ogg_muxer: String,
}

impl FfmpegPreflight {
    /// Returns true when the named video encoder was detected during preflight.
    #[must_use]
    pub fn has_encoder(&self, encoder: &str) -> bool {
        match encoder {
            "libx265" => !self.hevc_encoder.is_empty(),
            "libsvtav1" => !self.svtav1_encoder.is_empty(),
            "libaom-av1" => !self.libaom_encoder.is_empty(),
            "aac" => !self.aac_encoder.is_empty(),
            "libopus" => !self.opus_encoder.is_empty(),
            _ => false,
        }
    }

    /// Returns true when the named muxer was detected during preflight.
    #[must_use]
    pub fn has_muxer(&self, muxer: &str) -> bool {
        match muxer {
            "matroska" | "mkv" => !self.matroska_muxer.is_empty(),
            "mp4" => !self.mp4_muxer.is_empty(),
            "ogg" => !self.ogg_muxer.is_empty(),
            _ => false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FFmpegPreflightError {
    #[error("ffmpeg preflight failed: {0}")]
    Failed(String),
}

pub const FFMPEG_BIN_ENV: &str = "VOOM_FFMPEG_BIN";
pub const FFPROBE_BIN_ENV: &str = "VOOM_FFPROBE_BIN";
const DEFAULT_FFMPEG_BIN: &str = "ffmpeg";
const DEFAULT_FFPROBE_BIN: &str = "ffprobe";

pub fn preflight_from_process_env() -> Result<FfmpegPreflight, FFmpegPreflightError> {
    let ffmpeg =
        std::env::var_os(FFMPEG_BIN_ENV).unwrap_or_else(|| OsString::from(DEFAULT_FFMPEG_BIN));
    let ffprobe =
        std::env::var_os(FFPROBE_BIN_ENV).unwrap_or_else(|| OsString::from(DEFAULT_FFPROBE_BIN));
    let ffmpeg_path = resolve_binary(&ffmpeg);
    let ffprobe_path = resolve_binary(&ffprobe);
    preflight_with_paths(&ffmpeg_path, &ffprobe_path)
}

pub fn preflight_with_paths(
    ffmpeg_path: &Path,
    ffprobe_path: &Path,
) -> Result<FfmpegPreflight, FFmpegPreflightError> {
    require_executable_file("ffmpeg", ffmpeg_path)?;
    require_executable_file("ffprobe", ffprobe_path)?;

    let ffmpeg_version = first_output_line(
        "ffmpeg -hide_banner -version",
        command_output(
            Command::new(ffmpeg_path)
                .arg("-hide_banner")
                .arg("-version"),
        ),
    )?;
    let ffprobe_version = first_output_line(
        "ffprobe -hide_banner -version",
        command_output(
            Command::new(ffprobe_path)
                .arg("-hide_banner")
                .arg("-version"),
        ),
    )?;
    let encoders = command_text(
        "ffmpeg -hide_banner -encoders",
        command_output(
            Command::new(ffmpeg_path)
                .arg("-hide_banner")
                .arg("-encoders"),
        ),
    )?;
    let hevc_encoder = parse_libx265_encoder(&encoders).ok_or_else(|| {
        FFmpegPreflightError::Failed(
            "ffmpeg does not advertise required libx265 encoder".to_owned(),
        )
    })?;
    let svtav1_encoder = parse_token(&encoders, "libsvtav1").ok_or_else(|| {
        FFmpegPreflightError::Failed(
            "ffmpeg does not advertise required libsvtav1 encoder".to_owned(),
        )
    })?;
    let libaom_encoder = parse_token(&encoders, "libaom-av1").ok_or_else(|| {
        FFmpegPreflightError::Failed(
            "ffmpeg does not advertise required libaom-av1 encoder".to_owned(),
        )
    })?;
    let aac_encoder = parse_token(&encoders, "aac").ok_or_else(|| {
        FFmpegPreflightError::Failed("ffmpeg does not advertise required aac encoder".to_owned())
    })?;
    let opus_encoder = parse_token(&encoders, "libopus").ok_or_else(|| {
        FFmpegPreflightError::Failed(
            "ffmpeg does not advertise required libopus encoder".to_owned(),
        )
    })?;
    let muxers = command_text(
        "ffmpeg -hide_banner -muxers",
        command_output(Command::new(ffmpeg_path).arg("-hide_banner").arg("-muxers")),
    )?;
    let matroska_muxer = parse_token(&muxers, "matroska").ok_or_else(|| {
        FFmpegPreflightError::Failed("ffmpeg does not advertise required matroska muxer".to_owned())
    })?;
    let mp4_muxer = parse_token(&muxers, "mp4").ok_or_else(|| {
        FFmpegPreflightError::Failed("ffmpeg does not advertise required mp4 muxer".to_owned())
    })?;
    let ogg_muxer = parse_token(&muxers, "ogg").ok_or_else(|| {
        FFmpegPreflightError::Failed("ffmpeg does not advertise required ogg muxer".to_owned())
    })?;

    Ok(FfmpegPreflight {
        ffmpeg_path: ffmpeg_path.to_owned(),
        ffprobe_path: ffprobe_path.to_owned(),
        ffmpeg_version,
        ffprobe_version,
        hevc_encoder,
        svtav1_encoder,
        libaom_encoder,
        aac_encoder,
        opus_encoder,
        matroska_muxer,
        mp4_muxer,
        ogg_muxer,
    })
}

fn resolve_binary(binary: &OsStr) -> PathBuf {
    let path = PathBuf::from(binary);
    if path.components().count() > 1 {
        return path;
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return path;
    };
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(&path);
        if is_executable_file(&candidate) {
            return candidate;
        }
    }
    path
}

fn require_executable_file(label: &str, path: &Path) -> Result<(), FFmpegPreflightError> {
    if !is_executable_file(path) {
        return Err(FFmpegPreflightError::Failed(format!(
            "{label} binary is missing or not executable: {}",
            path.display()
        )));
    }
    Ok(())
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    is_executable_metadata(&metadata)
}

#[cfg(unix)]
fn is_executable_metadata(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable_metadata(_metadata: &std::fs::Metadata) -> bool {
    true
}

fn first_output_line(
    command_name: &str,
    output: std::io::Result<std::process::Output>,
) -> Result<String, FFmpegPreflightError> {
    command_text(command_name, output)?
        .lines()
        .next()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| FFmpegPreflightError::Failed(format!("{command_name} produced no output")))
}

fn command_text(
    command_name: &str,
    output: std::io::Result<std::process::Output>,
) -> Result<String, FFmpegPreflightError> {
    let output = output.map_err(|err| {
        FFmpegPreflightError::Failed(format!("{command_name} failed to start: {err}"))
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = format!("{stdout}{stderr}");
    if output.status.success() {
        Ok(text)
    } else {
        Err(FFmpegPreflightError::Failed(format!(
            "{command_name} exited with status {}: {}",
            output
                .status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            text.trim()
        )))
    }
}

fn command_output(command: &mut Command) -> io::Result<Output> {
    for attempt in 0..3 {
        match command.output() {
            Err(err) if is_text_file_busy(&err) && attempt < 2 => {
                thread::sleep(Duration::from_millis(10));
            }
            result => return result,
        }
    }
    command.output()
}

fn is_text_file_busy(err: &io::Error) -> bool {
    err.raw_os_error() == Some(26)
}

fn parse_libx265_encoder(encoders: &str) -> Option<String> {
    parse_token(encoders, "libx265")
}

fn parse_token(text: &str, token: &str) -> Option<String> {
    text.lines()
        .find(|line| line.split_whitespace().any(|candidate| candidate == token))
        .map(|_| token.to_owned())
}

#[cfg(test)]
#[path = "preflight_test.rs"]
mod tests;
