#![allow(
    dead_code,
    reason = "published grammar media helpers grow with the serial corpus test"
)]

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;

use crate::media_inspect::{assert_stream_tone, ffprobe, mkvmerge_identify};
use crate::process::{BoundedOutput, run_bounded};

const TOOL_TIMEOUT: Duration = Duration::from_mins(2);
pub const SURROUND_TONE_HZ: f64 = 440.0;
pub const ENGLISH_TONE_HZ: f64 = 550.0;
pub const UNTAGGED_TONE_HZ: f64 = 660.0;
pub const COMMENTARY_TONE_HZ: f64 = 880.0;
pub const ALL_TONES_HZ: [f64; 4] = [
    SURROUND_TONE_HZ,
    ENGLISH_TONE_HZ,
    UNTAGGED_TONE_HZ,
    COMMENTARY_TONE_HZ,
];

pub struct GeneratedCorpus {
    pub core: ScenarioMedia,
    pub tracks: ScenarioMedia,
    pub audio: ScenarioMedia,
    pub flow: ScenarioMedia,
}

pub struct ScenarioMedia {
    _temp: TempDir,
    pub root: PathBuf,
    pub library: PathBuf,
    pub scratch: PathBuf,
    pub files: BTreeMap<&'static str, PathBuf>,
}

struct SharedTracks {
    surround: PathBuf,
    english: PathBuf,
    untagged: PathBuf,
    commentary: PathBuf,
    signs: PathBuf,
    untagged_subtitle: PathBuf,
    forced: PathBuf,
}

struct MkvTrack<'a> {
    path: &'a Path,
    language: &'static str,
    name: Option<&'static str>,
    default: bool,
    forced: bool,
    commentary: bool,
}

pub fn generate_and_validate_all() -> Result<GeneratedCorpus, Box<dyn Error>> {
    let core = generate_core()?;
    validate_core(&core)?;
    let tracks = generate_tracks()?;
    validate_tracks(&tracks)?;
    let audio = generate_audio_policy_media()?;
    validate_audio(&audio)?;
    let flow = generate_control_flow()?;
    validate_control_flow(&flow)?;
    Ok(GeneratedCorpus {
        core,
        tracks,
        audio,
        flow,
    })
}

impl ScenarioMedia {
    fn new(prefix: &str) -> io::Result<Self> {
        let temp = tempfile::Builder::new().prefix(prefix).tempdir()?;
        let root = temp.path().canonicalize()?;
        let library = root.join("library");
        let scratch = root.join("scratch");
        std::fs::create_dir(&library)?;
        std::fs::create_dir(&scratch)?;
        Ok(Self {
            _temp: temp,
            root,
            library,
            scratch,
            files: BTreeMap::new(),
        })
    }

    pub fn file(&self, key: &str) -> io::Result<&Path> {
        self.files
            .get(key)
            .map(PathBuf::as_path)
            .ok_or_else(|| io::Error::other(format!("missing generated media key {key}")))
    }

    fn insert(&mut self, key: &'static str, file_name: &'static str) -> PathBuf {
        let path = self.library.join(file_name);
        self.files.insert(key, path.clone());
        path
    }
}

fn generate_core() -> io::Result<ScenarioMedia> {
    let mut scenario = ScenarioMedia::new("voom-published-core-")?;
    let target = scenario.insert("c1", "core.mp4");
    let mut command = ffmpeg();
    command
        .args(["-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=2"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"])
        .args(["-t", "2", "-map", "0:v:0", "-map", "1:a:0"])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ])
        .args(["-c:a", "aac", "-ac", "2"])
        .args([
            "-metadata:s:a:0",
            "language=eng",
            "-disposition:a:0",
            "default",
        ])
        .arg(&target);
    run_success(&mut command, "generate C1")?;
    Ok(scenario)
}

fn generate_tracks() -> io::Result<ScenarioMedia> {
    let mut scenario = ScenarioMedia::new("voom-published-tracks-")?;
    let shared = generate_shared_tracks(&scenario.scratch, "tracks")?;
    let variants = [
        ("t1a", "tracks-1920.mkv", 1920, 1080),
        ("t1b", "tracks-1024.mkv", 1024, 576),
        ("t1c", "tracks-512.mkv", 512, 288),
    ];
    for (key, file_name, width, height) in variants {
        let video = scenario.scratch.join(format!("{key}-video.mkv"));
        generate_video(&video, width, height, "h264", None)?;
        let target = scenario.insert(key, file_name);
        assemble_track_policy_mkv(&target, &video, &shared, &scenario.scratch)?;
    }
    Ok(scenario)
}

fn generate_audio_policy_media() -> io::Result<ScenarioMedia> {
    let mut scenario = ScenarioMedia::new("voom-published-audio-")?;
    let shared = generate_shared_tracks(&scenario.scratch, "audio")?;
    let video = scenario.scratch.join("audio-video.mkv");
    generate_video(&video, 1280, 720, "h264", None)?;
    let target = scenario.insert("a1", "audio.mkv");
    let tracks = [
        video_track(&video),
        audio_track(&shared.surround, "eng", "Surround", true, false),
        audio_track(&shared.english, "eng", "Main", false, false),
        audio_track(&shared.untagged, "und", "Untagged", false, false),
        audio_track(
            &shared.commentary,
            "jpn",
            "Japanese Commentary",
            false,
            true,
        ),
    ];
    assemble_mkv(&target, &tracks, &[])?;
    Ok(scenario)
}

fn generate_control_flow() -> io::Result<ScenarioMedia> {
    let mut scenario = ScenarioMedia::new("voom-published-flow-")?;
    let modify = scenario.insert("f1a", "modify.mp4");
    generate_flow_mp4(&scenario.scratch, &modify, "modify", 330, 440)?;
    let normalized = scenario.insert("f1b", "already-normalized.mkv");
    generate_normalized_mkv(&scenario.scratch, &normalized)?;
    let fail = scenario.insert("f1c", "fail.mp4");
    generate_flow_mp4(&scenario.scratch, &fail, "fail", 770, 990)?;
    Ok(scenario)
}

fn generate_shared_tracks(root: &Path, prefix: &str) -> io::Result<SharedTracks> {
    let surround = root.join(format!("{prefix}-surround.eac3"));
    let english = root.join(format!("{prefix}-english.m4a"));
    let untagged = root.join(format!("{prefix}-untagged.m4a"));
    let commentary = root.join(format!("{prefix}-commentary.m4a"));
    generate_audio(&surround, 440, "eac3", 6)?;
    generate_audio(&english, 550, "aac", 2)?;
    generate_audio(&untagged, 660, "aac", 2)?;
    generate_audio(&commentary, 880, "aac", 2)?;
    let signs = root.join(format!("{prefix}-signs.srt"));
    let untagged_subtitle = root.join(format!("{prefix}-untagged.srt"));
    let forced = root.join(format!("{prefix}-forced.srt"));
    write_subtitle(&signs, "Signs")?;
    write_subtitle(&untagged_subtitle, "Untagged")?;
    write_subtitle(&forced, "Forced")?;
    Ok(SharedTracks {
        surround,
        english,
        untagged,
        commentary,
        signs,
        untagged_subtitle,
        forced,
    })
}

fn assemble_track_policy_mkv(
    target: &Path,
    video: &Path,
    shared: &SharedTracks,
    scratch: &Path,
) -> io::Result<()> {
    let font = scratch.join("fixture-font.ttf");
    let other = scratch.join("fixture-data.bin");
    std::fs::write(&font, b"deterministic font fixture payload\n")?;
    std::fs::write(&other, b"deterministic non-font fixture payload\n")?;
    let tracks = [
        audio_track(&shared.english, "eng", "Main", false, false),
        video_track(video),
        subtitle_track(&shared.signs, "eng", "Signs", false, false),
        audio_track(&shared.surround, "eng", "Surround", true, false),
        subtitle_track(&shared.untagged_subtitle, "und", "Untagged", false, false),
        audio_track(&shared.commentary, "eng", "English Commentary", false, true),
        subtitle_track(&shared.forced, "eng", "Forced", false, true),
        audio_track(&shared.untagged, "und", "Untagged", false, false),
    ];
    assemble_mkv(
        target,
        &tracks,
        &[(&font, "font/ttf"), (&other, "application/octet-stream")],
    )
}

fn assemble_mkv(
    target: &Path,
    tracks: &[MkvTrack<'_>],
    attachments: &[(&Path, &str)],
) -> io::Result<()> {
    let mut command = Command::new("mkvmerge");
    command.arg("-o").arg(target);
    for track in tracks {
        add_mkv_track(&mut command, track);
    }
    for (path, mime_type) in attachments {
        command
            .args(["--attachment-mime-type", mime_type, "--attach-file"])
            .arg(path);
    }
    let order = (0..tracks.len())
        .map(|index| format!("{index}:0"))
        .collect::<Vec<_>>()
        .join(",");
    command.args(["--track-order", &order]);
    run_success(&mut command, "assemble Matroska")
}

fn add_mkv_track(command: &mut Command, track: &MkvTrack<'_>) {
    command
        .args(["--language", &format!("0:{}", track.language)])
        .args([
            "--default-track-flag",
            if track.default { "0:yes" } else { "0:no" },
        ])
        .args([
            "--forced-display-flag",
            if track.forced { "0:yes" } else { "0:no" },
        ])
        .args([
            "--commentary-flag",
            if track.commentary { "0:yes" } else { "0:no" },
        ]);
    if let Some(name) = track.name {
        command.args(["--track-name", &format!("0:{name}")]);
    }
    command.arg(track.path);
}

fn generate_flow_mp4(
    scratch: &Path,
    target: &Path,
    prefix: &str,
    first_tone: u32,
    second_tone: u32,
) -> io::Result<()> {
    let video = scratch.join(format!("{prefix}-video.mp4"));
    let first_audio = scratch.join(format!("{prefix}-first.m4a"));
    let second_audio = scratch.join(format!("{prefix}-second.m4a"));
    let first_subtitle = scratch.join(format!("{prefix}-first.srt"));
    let second_subtitle = scratch.join(format!("{prefix}-second.srt"));
    generate_video(&video, 1920, 1080, "h264", Some("2M"))?;
    generate_audio(&first_audio, first_tone, "aac", 2)?;
    generate_audio(&second_audio, second_tone, "aac", 2)?;
    write_subtitle(&first_subtitle, "Main")?;
    write_subtitle(&second_subtitle, "Forced")?;
    assemble_mp4(
        target,
        &video,
        [&first_audio, &second_audio],
        [&first_subtitle, &second_subtitle],
    )
}

fn assemble_mp4(
    target: &Path,
    video: &Path,
    audio: [&Path; 2],
    subtitles: [&Path; 2],
) -> io::Result<()> {
    let mut command = ffmpeg();
    command
        .arg("-i")
        .arg(video)
        .arg("-i")
        .arg(audio[0])
        .arg("-i")
        .arg(audio[1])
        .arg("-i")
        .arg(subtitles[0])
        .arg("-i")
        .arg(subtitles[1])
        .args(["-map", "0:v:0", "-map", "1:a:0", "-map", "2:a:0"])
        .args(["-map", "3:s:0", "-map", "4:s:0"])
        .args(["-c:v", "copy", "-c:a", "copy", "-c:s", "mov_text"])
        .args([
            "-metadata:s:a:0",
            "language=eng",
            "-disposition:a:0",
            "default",
        ])
        .args(["-metadata:s:a:1", "language=und", "-disposition:a:1", "0"])
        .args([
            "-metadata:s:s:0",
            "language=eng",
            "-disposition:s:0",
            "default",
        ])
        .args([
            "-metadata:s:s:1",
            "language=eng",
            "-disposition:s:1",
            "forced",
        ])
        .arg(target);
    run_success(&mut command, "assemble F1 MP4")
}

fn generate_normalized_mkv(scratch: &Path, target: &Path) -> io::Result<()> {
    let video = scratch.join("normalized-video.mkv");
    let first_audio = scratch.join("normalized-first.m4a");
    let second_audio = scratch.join("normalized-second.m4a");
    let first_subtitle = scratch.join("normalized-first.srt");
    let second_subtitle = scratch.join("normalized-second.srt");
    generate_video(&video, 1920, 1080, "hevc", Some("2M"))?;
    generate_audio(&first_audio, 330, "aac", 2)?;
    generate_audio(&second_audio, 440, "aac", 2)?;
    write_subtitle(&first_subtitle, "Main")?;
    write_subtitle(&second_subtitle, "Forced")?;
    let tracks = [
        video_track(&video),
        audio_track(&first_audio, "eng", "Main", true, false),
        audio_track(&second_audio, "und", "Alternate", false, false),
        subtitle_track(&first_subtitle, "eng", "Main", true, false),
        subtitle_track(&second_subtitle, "eng", "Forced", false, true),
    ];
    assemble_mkv(target, &tracks, &[])
}

fn generate_video(
    target: &Path,
    width: u32,
    height: u32,
    codec: &str,
    bitrate: Option<&str>,
) -> io::Result<()> {
    let source = format!("testsrc2=size={width}x{height}:rate=2");
    let encoder = match codec {
        "h264" => "libx264",
        "hevc" => "libx265",
        other => return Err(io::Error::other(format!("unsupported video codec {other}"))),
    };
    let mut command = ffmpeg();
    command
        .args(["-f", "lavfi", "-i", &source, "-t", "2"])
        .args([
            "-c:v",
            encoder,
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ]);
    if let Some(bitrate) = bitrate {
        command.args([
            "-b:v", bitrate, "-minrate", bitrate, "-maxrate", bitrate, "-bufsize", "4M",
        ]);
    }
    command.arg(target);
    run_success(&mut command, "generate video")
}

fn generate_audio(target: &Path, frequency: u32, codec: &str, channels: u32) -> io::Result<()> {
    let source = format!("sine=frequency={frequency}:sample_rate=48000");
    let channels = channels.to_string();
    let mut command = ffmpeg();
    command
        .args(["-f", "lavfi", "-i", &source, "-t", "2", "-vn"])
        .args(["-c:a", codec, "-ac", &channels])
        .arg(target);
    run_success(&mut command, "generate audio")
}

fn write_subtitle(target: &Path, text: &str) -> io::Result<()> {
    std::fs::write(
        target,
        format!("1\n00:00:00,000 --> 00:00:01,500\n{text}\n"),
    )
}

fn validate_core(scenario: &ScenarioMedia) -> io::Result<()> {
    let probe = ffprobe(scenario.file("c1")?)?;
    assert_container(&probe, "mp4")?;
    let video = only_stream(&probe, "video")?;
    require(video["codec_name"] == "h264", "C1 video codec must be h264")?;
    require(video["width"] == 1920, "C1 width must be 1920")?;
    require(video["height"] == 1080, "C1 height must be 1080")?;
    let audio = only_stream(&probe, "audio")?;
    require(audio["codec_name"] == "aac", "C1 audio codec must be aac")?;
    require(audio["channels"] == 2, "C1 audio must be stereo")?;
    require(audio["tags"]["language"] == "eng", "C1 audio language")?;
    require(audio["disposition"]["default"] == 1, "C1 audio default")?;
    Ok(())
}

fn validate_tracks(scenario: &ScenarioMedia) -> io::Result<()> {
    for (key, width, height) in [("t1a", 1920, 1080), ("t1b", 1024, 576), ("t1c", 512, 288)] {
        let identified = mkvmerge_identify(scenario.file(key)?)?;
        let tracks = identified["tracks"]
            .as_array()
            .ok_or_else(|| io::Error::other(format!("{key} missing tracks")))?;
        require(tracks.len() == 8, format!("{key} must have eight tracks"))?;
        require(
            track_types(tracks) == expected_t1_types(),
            format!("{key} track order"),
        )?;
        let video = tracks
            .iter()
            .find(|track| track["type"] == "video")
            .ok_or_else(|| io::Error::other(format!("{key} missing video")))?;
        require(
            video["properties"]["pixel_dimensions"] == format!("{width}x{height}"),
            format!(
                "{key} dimensions: expected {width}x{height}, got {}",
                video["properties"]["pixel_dimensions"]
            ),
        )?;
        validate_t1_properties(tracks, key)?;
        let attachments = identified["attachments"]
            .as_array()
            .ok_or_else(|| io::Error::other(format!("{key} missing attachments")))?;
        let mime_types = attachments
            .iter()
            .filter_map(|item| item["content_type"].as_str())
            .collect::<Vec<_>>();
        require(
            mime_types == ["font/ttf", "application/octet-stream"],
            format!("{key} attachment MIME types: {mime_types:?}"),
        )?;
    }
    Ok(())
}

fn validate_t1_properties(tracks: &[Value], key: &str) -> io::Result<()> {
    require(
        track_language(&tracks[0]) == Some("eng"),
        format!("{key} English stereo language"),
    )?;
    require(
        track_name(&tracks[2]) == Some("Signs"),
        format!("{key} Signs title"),
    )?;
    require(
        track_flag(&tracks[3], "default_track"),
        format!("{key} surround default"),
    )?;
    require(
        track_language(&tracks[4]) == Some("und"),
        format!("{key} untagged subtitle language"),
    )?;
    require(
        track_flag(&tracks[5], "flag_commentary"),
        format!("{key} commentary flag: {}", tracks[5]["properties"]),
    )?;
    require(
        track_flag(&tracks[6], "forced_track"),
        format!("{key} forced flag"),
    )?;
    require(
        track_language(&tracks[7]) == Some("und"),
        format!("{key} untagged audio language"),
    )
}

fn validate_audio(scenario: &ScenarioMedia) -> io::Result<()> {
    let path = scenario.file("a1")?;
    let identified = mkvmerge_identify(path)?;
    let tracks = identified["tracks"]
        .as_array()
        .ok_or_else(|| io::Error::other("A1 missing tracks"))?;
    require(
        track_types(tracks) == ["video", "audio", "audio", "audio", "audio"],
        "A1 track order",
    )?;
    require(
        track_flag(&tracks[1], "default_track"),
        "A1 surround default",
    )?;
    require(
        track_language(&tracks[3]) == Some("und"),
        "A1 untagged language",
    )?;
    require(
        track_language(&tracks[4]) == Some("jpn"),
        "A1 commentary language",
    )?;
    require(
        track_flag(&tracks[4], "flag_commentary"),
        "A1 commentary flag",
    )?;
    assert_stream_tone(path, 1, SURROUND_TONE_HZ, &ALL_TONES_HZ)?;
    assert_stream_tone(path, 2, ENGLISH_TONE_HZ, &ALL_TONES_HZ)?;
    assert_stream_tone(path, 3, UNTAGGED_TONE_HZ, &ALL_TONES_HZ)?;
    assert_stream_tone(path, 4, COMMENTARY_TONE_HZ, &ALL_TONES_HZ)
}

fn validate_control_flow(scenario: &ScenarioMedia) -> io::Result<()> {
    for (key, container, codec) in [
        ("f1a", "mp4", "h264"),
        ("f1b", "matroska", "hevc"),
        ("f1c", "mp4", "h264"),
    ] {
        let probe = ffprobe(scenario.file(key)?)?;
        assert_container(&probe, container)?;
        let video = only_stream(&probe, "video")?;
        require(video["codec_name"] == codec, format!("{key} video codec"))?;
        require(video["width"] == 1920, format!("{key} width"))?;
        require(video["height"] == 1080, format!("{key} height"))?;
        require(
            stream_count(&probe, "audio") == 2,
            format!("{key} audio count"),
        )?;
        require(
            stream_count(&probe, "subtitle") == 2,
            format!("{key} subtitle count"),
        )?;
        let duration = probe["format"]["duration"]
            .as_str()
            .and_then(|value| value.parse::<f64>().ok())
            .ok_or_else(|| io::Error::other(format!("{key} missing duration")))?;
        require(duration >= 1.9, format!("{key} duration"))?;
        let bitrate = probe["format"]["bit_rate"]
            .as_str()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| io::Error::other(format!("{key} missing bitrate")))?;
        require(bitrate > 1_000_000, format!("{key} bitrate {bitrate}"))?;
    }
    Ok(())
}

fn only_stream<'a>(probe: &'a Value, kind: &str) -> io::Result<&'a Value> {
    let matches = probe["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|stream| stream["codec_type"] == kind)
        .collect::<Vec<_>>();
    if matches.len() == 1 {
        Ok(matches[0])
    } else {
        Err(io::Error::other(format!(
            "expected one {kind} stream, found {}",
            matches.len()
        )))
    }
}

fn stream_count(probe: &Value, kind: &str) -> usize {
    probe["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|stream| stream["codec_type"] == kind)
        .count()
}

fn assert_container(probe: &Value, expected: &str) -> io::Result<()> {
    let formats = probe["format"]["format_name"]
        .as_str()
        .ok_or_else(|| io::Error::other("ffprobe missing format_name"))?;
    require(
        formats.split(',').any(|format| format == expected),
        format!("expected container {expected}, got {formats}"),
    )
}

fn track_types(tracks: &[Value]) -> Vec<&str> {
    tracks
        .iter()
        .filter_map(|track| track["type"].as_str())
        .collect()
}

fn expected_t1_types() -> Vec<&'static str> {
    vec![
        "audio",
        "video",
        "subtitles",
        "audio",
        "subtitles",
        "audio",
        "subtitles",
        "audio",
    ]
}

fn track_language(track: &Value) -> Option<&str> {
    track["properties"]["language"].as_str()
}

fn track_name(track: &Value) -> Option<&str> {
    track["properties"]["track_name"].as_str()
}

fn track_flag(track: &Value, key: &str) -> bool {
    track["properties"][key].as_bool().unwrap_or(false)
}

fn video_track(path: &Path) -> MkvTrack<'_> {
    MkvTrack {
        path,
        language: "und",
        name: None,
        default: true,
        forced: false,
        commentary: false,
    }
}

fn audio_track<'a>(
    path: &'a Path,
    language: &'static str,
    name: &'static str,
    default: bool,
    commentary: bool,
) -> MkvTrack<'a> {
    MkvTrack {
        path,
        language,
        name: Some(name),
        default,
        forced: false,
        commentary,
    }
}

fn subtitle_track<'a>(
    path: &'a Path,
    language: &'static str,
    name: &'static str,
    default: bool,
    forced: bool,
) -> MkvTrack<'a> {
    MkvTrack {
        path,
        language,
        name: Some(name),
        default,
        forced,
        commentary: false,
    }
}

fn ffmpeg() -> Command {
    let mut command = Command::new("ffmpeg");
    command.args(["-y", "-hide_banner", "-loglevel", "error", "-nostdin"]);
    command
}

fn run_success(command: &mut Command, what: &str) -> io::Result<()> {
    let output = run_bounded(command, TOOL_TIMEOUT)?;
    require_process_success(&output, what)
}

fn require_process_success(output: &BoundedOutput, what: &str) -> io::Result<()> {
    if !output.timed_out && output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(output.diagnostics(what)))
    }
}

fn require(condition: bool, message: impl AsRef<OsStr>) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message.as_ref().to_string_lossy()))
    }
}
