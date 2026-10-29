use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Stdio;

use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use voom_worker_protocol::AudioStreamRef;

use super::{FfmpegConfig, FfmpegError, audio_stream_values, probe_json};

const LINE_LIMIT: u64 = 1024;
const STDERR_LIMIT: u64 = 16_384;
type Fingerprint = (u64, [u8; 32]);

fn invalid(detail: &str) -> FfmpegError {
    FfmpegError::OutputFactsMismatch(format!("copied audio preservation: {detail}"))
}

#[derive(Clone, Copy)]
struct TimeBase(i64, i64);

impl TimeBase {
    fn parse(value: &str) -> Result<Self, FfmpegError> {
        let (numerator, denominator) = value
            .split_once('/')
            .ok_or_else(|| invalid("invalid time base"))?;
        let numerator = numerator
            .parse::<i64>()
            .map_err(|_| invalid("invalid time base"))?;
        let denominator = denominator
            .parse::<i64>()
            .map_err(|_| invalid("invalid time base"))?;
        if numerator <= 0 || denominator <= 0 {
            return Err(invalid("nonpositive time base"));
        }
        Ok(Self(numerator, denominator))
    }

    fn time(self, value: Option<&str>) -> Result<Option<(i128, i128)>, FfmpegError> {
        let Some(value) = value.filter(|value| *value != "N/A") else {
            return Ok(None);
        };
        let value = value
            .parse::<i64>()
            .map_err(|_| invalid("invalid packet timing"))?;
        // The product of two i64 values fits in i128, including negative timestamps.
        let numerator = i128::from(value) * i128::from(self.0);
        let denominator = i128::from(self.1);
        let mut a = numerator.abs();
        let mut b = denominator;
        while b != 0 {
            (a, b) = (b, a % b);
        }
        Ok(Some((numerator / a, denominator / a)))
    }
}

struct Stream {
    index: u64,
    time_base: TimeBase,
    count: u64,
    hash: Sha256,
}

impl Stream {
    fn new(value: &Value) -> Result<Self, FfmpegError> {
        Ok(Self {
            index: stream_index(value)?,
            time_base: TimeBase::parse(
                value["time_base"]
                    .as_str()
                    .ok_or_else(|| invalid("missing stream time base"))?,
            )?,
            count: 0,
            hash: Sha256::new(),
        })
    }

    fn packet(&mut self, fields: &BTreeMap<&str, &str>) -> Result<(), FfmpegError> {
        let hash = fields
            .get("data_hash")
            .and_then(|value| value.strip_prefix("SHA256:"))
            .ok_or_else(|| invalid("missing SHA256 packet hash"))?;
        let mut bytes = [0; 32];
        hex::decode_to_slice(hash, &mut bytes)
            .map_err(|_| invalid("invalid SHA256 packet hash"))?;
        self.hash.update(bytes);
        for field in ["pts", "dts", "duration"] {
            if let Some((numerator, denominator)) =
                self.time_base.time(fields.get(field).copied())?
            {
                self.hash.update([1]);
                self.hash.update(numerator.to_be_bytes());
                self.hash.update(denominator.to_be_bytes());
            } else {
                self.hash.update([0]);
            }
        }
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| invalid("packet count overflow"))?;
        Ok(())
    }
}

fn stream_index(value: &Value) -> Result<u64, FfmpegError> {
    value["index"]
        .as_u64()
        .ok_or_else(|| invalid("missing audio stream index"))
}

pub(super) async fn verify(
    config: &FfmpegConfig,
    input: &Path,
    output: &Path,
    selected: &[AudioStreamRef],
    synthesis: bool,
) -> Result<(), FfmpegError> {
    let source = probe_json(config, input).await?;
    let source: Vec<_> = audio_stream_values(&source).collect();
    let source_indices = source
        .iter()
        .map(|stream| stream_index(stream))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if source_indices.len() != source.len()
        || selected.is_empty()
        || selected
            .iter()
            .any(|stream| !source_indices.contains(&u64::from(stream.provider_stream_index)))
    {
        return Err(invalid("missing or duplicate source audio stream"));
    }
    let copied: Vec<_> = source
        .iter()
        .enumerate()
        .filter_map(|(ordinal, stream)| {
            let index = stream_index(stream);
            match index {
                Ok(index)
                    if synthesis
                        || !selected
                            .iter()
                            .any(|s| u64::from(s.provider_stream_index) == index) =>
                {
                    Some(Ok(ordinal))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect::<Result<_, _>>()?;
    if copied.is_empty() {
        return Ok(());
    }
    let destination = probe_json(config, output).await?;
    let destination: Vec<_> = audio_stream_values(&destination).collect();
    let expected_count = source
        .len()
        .checked_add(if synthesis { selected.len() } else { 0 })
        .ok_or_else(|| invalid("audio stream count overflow"))?;
    if destination.len() != expected_count {
        return Err(invalid("unexpected output audio stream count"));
    }
    let original = copied
        .iter()
        .map(|&ordinal| Stream::new(source[ordinal]))
        .collect::<Result<Vec<_>, _>>()?;
    let actual = copied
        .iter()
        .map(|&ordinal| Stream::new(destination[ordinal]))
        .collect::<Result<Vec<_>, _>>()?;
    let before = collect(config, input, original, &source).await?;
    let after = collect(config, output, actual, &destination).await?;
    if before != after {
        return Err(invalid(
            "packet payload/order/timing changed; use the paired project FFmpeg build with preserve_audio_duration",
        ));
    }
    Ok(())
}

fn fields(line: &str) -> Result<BTreeMap<&str, &str>, FfmpegError> {
    let mut fields = BTreeMap::new();
    // FFprobe emits one empty trailing side-data section even when suppressed.
    for field in line.strip_suffix('|').unwrap_or(line).split('|') {
        let (key, value) = field
            .split_once('=')
            .ok_or_else(|| invalid("invalid packet record"))?;
        if !matches!(
            key,
            "stream_index" | "pts" | "dts" | "duration" | "data_hash"
        ) || fields.insert(key, value).is_some()
        {
            return Err(invalid("unknown or duplicate packet field"));
        }
    }
    Ok(fields)
}

async fn packets(
    reader: impl AsyncRead + Unpin,
    mut streams: Vec<Stream>,
    known: BTreeSet<u64>,
) -> Result<Vec<Fingerprint>, FfmpegError> {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        let size = (&mut reader)
            .take(LINE_LIMIT + 1)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|_| invalid("packet read failed"))?;
        if size == 0 {
            break;
        }
        if size as u64 > LINE_LIMIT || line.pop() != Some(b'\n') {
            return Err(invalid("oversized or truncated packet record"));
        }
        let line = std::str::from_utf8(&line).map_err(|_| invalid("invalid packet text"))?;
        let fields = fields(line)?;
        let index = fields
            .get("stream_index")
            .ok_or_else(|| invalid("missing packet stream index"))?
            .parse::<u64>()
            .map_err(|_| invalid("invalid packet stream index"))?;
        if !known.contains(&index) {
            return Err(invalid("unknown packet stream index"));
        }
        if let Some(stream) = streams.iter_mut().find(|stream| stream.index == index) {
            stream.packet(&fields)?;
        }
    }
    Ok(streams
        .into_iter()
        .map(|stream| (stream.count, stream.hash.finalize().into()))
        .collect())
}

async fn stderr(reader: impl AsyncRead + Unpin) -> Result<(), FfmpegError> {
    let mut bytes = Vec::new();
    reader
        .take(STDERR_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| invalid("probe diagnostic read failed"))?;
    if bytes.len() as u64 > STDERR_LIMIT {
        return Err(invalid("probe diagnostic limit exceeded"));
    }
    if !bytes.is_empty() {
        return Err(invalid(
            "packet probe reported errors; check media integrity and configured ffprobe",
        ));
    }
    Ok(())
}

async fn collect(
    config: &FfmpegConfig,
    path: &Path,
    streams: Vec<Stream>,
    metadata: &[&Value],
) -> Result<Vec<Fingerprint>, FfmpegError> {
    let known = metadata
        .iter()
        .map(|stream| stream_index(stream))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if known.len() != metadata.len() {
        return Err(invalid("duplicate audio stream index"));
    }
    let mut child = Command::new(&config.ffprobe_path)
        .args([
            "-v",
            "error",
            "-select_streams",
            "a",
            "-show_packets",
            "-show_entries",
            "packet=stream_index,pts,dts,duration,data_hash:packet_side_data=",
            "-show_data_hash",
            "sha256",
            "-of",
            "compact=p=0:nk=0",
        ])
        .arg(path)
        .env("VOOM_MEDIA_CALLER_PID", std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| invalid("packet probe could not start"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| invalid("missing packet pipe"))?;
    let errors = child
        .stderr
        .take()
        .ok_or_else(|| invalid("missing diagnostic pipe"))?;
    let outcome = timeout(config.process_timeout, async {
        tokio::try_join!(packets(stdout, streams, known), stderr(errors), async {
            child
                .wait()
                .await
                .map_err(|_| invalid("packet probe wait failed"))
        })
    })
    .await;
    match outcome {
        Ok(Ok((packets, (), status))) if status.success() => Ok(packets),
        other => {
            timeout(Duration::from_secs(5), child.kill())
                .await
                .map_err(|_| invalid("packet probe cleanup timed out"))?
                .map_err(|_| invalid("packet probe cleanup failed"))?;
            match other {
                Err(_) => Err(invalid("packet probe timed out")),
                Ok(Err(error)) => Err(error),
                Ok(Ok(_)) => Err(invalid("packet probe exited unsuccessfully")),
            }
        }
    }
}

#[cfg(test)]
#[path = "copied_audio_test.rs"]
mod tests;
