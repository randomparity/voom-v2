//! Canonical comparisons for small, generated integer-audio integration fixtures.

use std::io;
use std::path::Path;
use std::process::Output;
use std::time::Duration;

use tokio::process::Command;
use voom_worker_protocol::{
    AudioSampleFacts, LosslessAudioSettings, ResolvedLosslessAudioSettings,
};

#[derive(Debug, Clone)]
pub struct CanonicalDecodedAudio {
    pub facts: ResolvedLosslessAudioSettings,
    pub samples: Vec<u8>,
}

impl CanonicalDecodedAudio {
    /// Compare integer samples and topology; representational padding may differ.
    ///
    /// # Errors
    /// Returns an error for different rate, channel order/count, or samples.
    pub fn compare_samples(&self, other: &Self) -> io::Result<()> {
        if self.facts.sample_rate != other.facts.sample_rate
            || self.facts.channels != other.facts.channels
            || self.facts.channel_layout != other.facts.channel_layout
        {
            return Err(io::Error::other(
                "decoded audio rate/channels/layout differ",
            ));
        }
        if self.samples != other.samples {
            return Err(io::Error::other("decoded audio samples differ"));
        }
        Ok(())
    }
}

/// Decode one stream to signed interleaved s32le without changing rate or layout.
///
/// For small generated fixtures only: complete decoded bytes are retained in memory.
/// Required tools fail loudly; callers own fixture temporary directories.
///
/// # Errors
/// Rejects missing tools, failed/slow commands, missing streams, unsupported or
/// unknown integer facts, empty audio, and incomplete interleaved frames.
pub async fn canonical_decoded_audio(
    ffmpeg: &Path,
    ffprobe: &Path,
    path: &Path,
    provider_stream_index: u32,
) -> io::Result<CanonicalDecodedAudio> {
    let probe = command_output(
        Command::new(ffprobe)
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .arg(path),
    )
    .await?;
    let json: serde_json::Value =
        serde_json::from_slice(&probe.stdout).map_err(io::Error::other)?;
    let stream = json
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .and_then(|streams| {
            streams.iter().find(|stream| {
                stream.get("index").and_then(serde_json::Value::as_u64)
                    == Some(u64::from(provider_stream_index))
                    && stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("audio")
            })
        })
        .ok_or_else(|| io::Error::other("selected audio stream is missing"))?;
    let observed = AudioSampleFacts::from_ffprobe(stream).map_err(io::Error::other)?;
    let facts = LosslessAudioSettings::default()
        .resolve(&observed)
        .map_err(io::Error::other)?;
    let output = command_output(
        Command::new(ffmpeg)
            .args(["-nostdin", "-v", "error", "-guess_layout_max", "0", "-i"])
            .arg(path)
            .args([
                "-map",
                &format!("0:{provider_stream_index}"),
                "-c:a",
                "pcm_s32le",
                "-dither_method",
                "0",
                "-f",
                "s32le",
                "pipe:1",
            ]),
    )
    .await?;
    let frame_bytes = usize::try_from(facts.channels).map_err(io::Error::other)? * 4;
    if output.stdout.is_empty() || output.stdout.len() % frame_bytes != 0 {
        return Err(io::Error::other(
            "decoded audio is empty or has incomplete frames",
        ));
    }
    Ok(CanonicalDecodedAudio {
        facts,
        samples: output.stdout,
    })
}

async fn command_output(command: &mut Command) -> io::Result<Output> {
    command.env("VOOM_MEDIA_CALLER_PID", std::process::id().to_string());
    let operation = format!("{command:?}");
    let output = tokio::time::timeout(Duration::from_secs(10), command.kill_on_drop(true).output())
        .await
        .map_err(|_| {
            io::Error::new(io::ErrorKind::TimedOut, format!("{operation}: timed out"))
        })??;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{operation}: {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output)
}

#[cfg(test)]
#[path = "audio_samples_test.rs"]
mod tests;
