#![allow(
    dead_code,
    reason = "media inspection helpers are shared across corpus scenario oracles"
)]

use std::f64::consts::PI;
use std::io;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::process::{BoundedOutput, run_bounded};

const TOOL_TIMEOUT: Duration = Duration::from_mins(2);
const SAMPLE_RATE: f64 = 8_000.0;
const SKIP_SAMPLES: usize = 1_024;
const WINDOW_SAMPLES: usize = 4_096;
const MIN_SAMPLES: usize = SKIP_SAMPLES + WINDOW_SAMPLES;
const TONE_RATIO: f64 = 8.0;

pub fn ffprobe(path: &Path) -> io::Result<Value> {
    let output = run_bounded(
        Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_format",
                "-show_streams",
                "-of",
                "json",
            ])
            .arg(path),
        TOOL_TIMEOUT,
    )?;
    parse_json(&output, "ffprobe")
}

pub fn mkvmerge_identify(path: &Path) -> io::Result<Value> {
    let output = run_bounded(Command::new("mkvmerge").arg("-J").arg(path), TOOL_TIMEOUT)?;
    parse_json(&output, "mkvmerge identify")
}

pub fn assert_stream_tone(
    path: &Path,
    stream_index: u64,
    expected_hz: f64,
    candidates_hz: &[f64],
) -> io::Result<()> {
    let samples = decode_pcm(path, stream_index)?;
    if samples.len() < MIN_SAMPLES {
        return Err(io::Error::other(format!(
            "{} stream {stream_index} decoded {} samples; need {MIN_SAMPLES}",
            path.display(),
            samples.len()
        )));
    }
    let window = &samples[SKIP_SAMPLES..MIN_SAMPLES];
    let expected = goertzel_energy(window, expected_hz);
    for candidate in candidates_hz {
        if (*candidate - expected_hz).abs() < f64::EPSILON {
            continue;
        }
        let other = goertzel_energy(window, *candidate);
        if expected < other * TONE_RATIO {
            return Err(io::Error::other(format!(
                "{} stream {stream_index}: {expected_hz}Hz energy {expected:.2} \
                 did not exceed {candidate}Hz energy {other:.2} by {TONE_RATIO}x",
                path.display()
            )));
        }
    }
    Ok(())
}

fn decode_pcm(path: &Path, stream_index: u64) -> io::Result<Vec<f64>> {
    let map = format!("0:{stream_index}");
    let output = run_bounded(
        Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-i"])
            .arg(path)
            .args(["-map", &map, "-ac", "1", "-ar", "8000", "-f", "s16le", "-"]),
        TOOL_TIMEOUT,
    )?;
    require_success(&output, "ffmpeg PCM decode")?;
    Ok(output
        .stdout
        .chunks_exact(2)
        .map(|bytes| f64::from(i16::from_le_bytes([bytes[0], bytes[1]])))
        .collect())
}

fn goertzel_energy(samples: &[f64], frequency: f64) -> f64 {
    let omega = 2.0 * PI * frequency / SAMPLE_RATE;
    let coefficient = 2.0 * omega.cos();
    let mut previous = 0.0;
    let mut previous_previous = 0.0;
    let denominator = f64::from(u32::try_from(samples.len() - 1).unwrap_or(u32::MAX));
    for (index, sample) in samples.iter().enumerate() {
        let index = f64::from(u32::try_from(index).unwrap_or(u32::MAX));
        let hann = 0.5 - 0.5 * (2.0 * PI * index / denominator).cos();
        let current = sample * hann + coefficient * previous - previous_previous;
        previous_previous = previous;
        previous = current;
    }
    previous * previous + previous_previous * previous_previous
        - coefficient * previous * previous_previous
}

fn parse_json(output: &BoundedOutput, what: &str) -> io::Result<Value> {
    require_success(output, what)?;
    serde_json::from_slice(&output.stdout).map_err(|err| {
        io::Error::other(format!(
            "{what} emitted invalid JSON: {err}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        ))
    })
}

fn require_success(output: &BoundedOutput, what: &str) -> io::Result<()> {
    if !output.timed_out && output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(output.diagnostics(what)))
    }
}
