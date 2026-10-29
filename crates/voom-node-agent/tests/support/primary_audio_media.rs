use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

pub struct AudioFixtureTrack<'a> {
    pub title: &'a str,
    pub language: Option<&'a str>,
    pub channels: u32,
    pub layout: &'a str,
    pub frequency: u32,
    pub commentary: bool,
    pub codec: &'a str,
}

pub fn dependency(name: &str) -> PathBuf {
    let paths = std::env::var_os("PATH").unwrap();
    let found = std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file());
    assert!(
        found.is_some(),
        "required generated-media dependency {name} is missing"
    );
    found.unwrap().canonicalize().unwrap()
}

pub fn generate_fixture(root: &Path, tracks: &[AudioFixtureTrack<'_>]) -> PathBuf {
    let subtitle = root.join("original.srt");
    std::fs::write(
        &subtitle,
        "1\n00:00:00,000 --> 00:00:00,400\nPreserve this subtitle\n",
    )
    .unwrap();
    let attachment = root.join("original.txt");
    std::fs::write(&attachment, b"Preserve this attachment\n").unwrap();
    let generated = root.join("generated.mkv");
    let path = root.join("source.mkv");
    let mut command = Command::new("ffmpeg");
    command.args([
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "color=size=16x16:rate=10",
    ]);
    for track in tracks {
        let expression = format!("0.1*sin(2*PI*{}*t)", track.frequency);
        let expressions = vec![expression; usize::try_from(track.channels).unwrap()].join("|");
        command.args(["-f", "lavfi", "-i"]).arg(format!(
            "aevalsrc=exprs={expressions}:s=32000:c={}",
            track.layout
        ));
    }
    command
        .arg("-i")
        .arg(&subtitle)
        .args(["-t", "0.5", "-map", "0:v"]);
    for index in 0..tracks.len() {
        command.arg("-map").arg(format!("{}:a", index + 1));
        if index == 0 {
            command.arg("-map").arg(format!("{}:s", tracks.len() + 1));
        }
    }
    if tracks.is_empty() {
        command.arg("-map").arg(format!("{}:s", tracks.len() + 1));
    }
    command.args([
        "-c:v",
        "ffv1",
        "-c:s",
        "srt",
        "-metadata:s:v:0",
        "title=Video",
        "-metadata:s:s:0",
        "title=Subtitle",
        "-disposition:v:0",
        "default+forced",
        "-disposition:s:0",
        "default+forced",
    ]);
    for (index, track) in tracks.iter().enumerate() {
        command
            .arg(format!("-c:a:{index}"))
            .arg(track.codec)
            .arg(format!("-metadata:s:a:{index}"))
            .arg(format!("title={}", track.title))
            .arg(format!("-disposition:a:{index}"))
            .arg(if track.commentary {
                "comment"
            } else if index == 0 {
                "default"
            } else {
                "0"
            });
        if track.codec == "wavpack" {
            // One complete frame prevents Matroska placement from lacing away packet timestamps.
            command.arg(format!("-frame_size:a:{index}")).arg("16000");
        }
        if let Some(language) = track.language {
            command
                .arg(format!("-metadata:s:a:{index}"))
                .arg(format!("language={language}"));
        }
    }
    command
        .arg("-attach")
        .arg(attachment)
        .args([
            "-metadata:s:t:0",
            "mimetype=text/plain",
            "-metadata:s:t:0",
            "filename=original.txt",
        ])
        .arg(&generated);
    successful(command);
    // Native FFmpeg 9 omits WavPack durations while copying. Construct that representation
    // before any scan observes this policy fixture; the worker must preserve its observations.
    // At 32 kHz, derived AAC frames also occupy exactly 32 ms through Matroska placement.
    let original_packets = packet_hashes(&generated);
    let mut command = Command::new("ffmpeg");
    command
        .args(["-v", "error", "-nostdin", "-n", "-copyts", "-i"])
        .arg(&generated)
        .args(["-map", "0", "-c", "copy", "-avoid_negative_ts", "disabled"])
        .arg(&path);
    successful(command);
    assert_eq!(packet_hashes(&path), original_packets);
    path
}

fn successful(mut command: Command) -> Vec<u8> {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "media fixture command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

pub fn probe(path: &Path) -> Value {
    let mut command = Command::new("ffprobe");
    command
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path);
    serde_json::from_slice(&successful(command)).unwrap()
}

pub fn tag(stream: &Value, name: &str) -> Option<String> {
    stream
        .get("tags")
        .and_then(Value::as_object)?
        .iter()
        .find_map(|(key, value)| {
            key.eq_ignore_ascii_case(name)
                .then(|| value.as_str().map(str::to_owned))
                .flatten()
        })
}

pub fn packet_hashes(path: &Path) -> BTreeMap<String, Vec<String>> {
    let mut command = Command::new("ffprobe");
    command
        .args([
            "-v",
            "error",
            "-show_packets",
            "-show_data_hash",
            "sha256",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(path);
    let data: Value = serde_json::from_slice(&successful(command)).unwrap();
    let names = data["streams"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|stream| {
            Some((
                stream["index"].as_u64()?,
                format!(
                    "{}|{}",
                    tag(stream, "title")?,
                    stream["codec_name"].as_str()?
                ),
            ))
        })
        .collect::<BTreeMap<_, _>>();
    let mut hashes = BTreeMap::<String, Vec<String>>::new();
    for packet in data["packets"].as_array().unwrap() {
        if let Some(name) = packet["stream_index"]
            .as_u64()
            .and_then(|index| names.get(&index))
        {
            hashes
                .entry(name.clone())
                .or_default()
                .push(packet["data_hash"].as_str().unwrap().to_owned());
        }
    }
    hashes
}

pub fn assert_selected_tone(path: &Path, index: u64, expected: u32, rejected: &[u32]) {
    let mut command = Command::new("ffmpeg");
    command
        .args(["-v", "error", "-i"])
        .arg(path)
        .arg("-map")
        .arg(format!("0:{index}"))
        .args([
            "-t", "0.4", "-ac", "1", "-ar", "8000", "-f", "f32le", "pipe:1",
        ]);
    let bytes = successful(command);
    let samples = bytes
        .chunks_exact(4)
        .map(|b| f64::from(f32::from_le_bytes(b.try_into().unwrap())))
        .collect::<Vec<_>>();
    let energy = |frequency: u32| {
        let (sin, cos) = samples
            .iter()
            .enumerate()
            .fold((0.0, 0.0), |(s, c), (i, sample)| {
                let phase = 2.0
                    * std::f64::consts::PI
                    * f64::from(frequency)
                    * f64::from(u32::try_from(i).unwrap())
                    / 8000.0;
                (s + sample * phase.sin(), c + sample * phase.cos())
            });
        sin.hypot(cos)
    };
    assert!(!samples.is_empty());
    for other in rejected {
        assert!(
            energy(expected) > 5.0 * energy(*other),
            "wrong source tone: expected {expected}, compared with {other}"
        );
    }
}

pub fn assert_attachment(path: &Path, root: &Path) {
    let mut command = Command::new("mkvmerge");
    command.arg("-J").arg(path);
    let data: Value = serde_json::from_slice(&successful(command)).unwrap();
    let attachments = data["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    let output = root.join("observed-attachment.txt");
    let mut command = Command::new("mkvextract");
    command.arg(path).arg("attachments").arg(format!(
        "{}:{}",
        attachments[0]["id"].as_u64().unwrap(),
        output.display()
    ));
    successful(command);
    assert_eq!(
        std::fs::read(output).unwrap(),
        b"Preserve this attachment\n"
    );
}

pub fn non_audio_state(probe: &Value) -> Vec<Value> {
    probe["streams"].as_array().unwrap().iter().filter(|stream| stream["codec_type"] != "audio")
        .map(|stream| serde_json::json!({
            "index": stream["index"], "kind": stream["codec_type"], "codec": stream["codec_name"],
            "disposition": stream["disposition"], "title": tag(stream, "title"),
            "filename": tag(stream, "filename"), "mimetype": tag(stream, "mimetype"),
        })).collect()
}
