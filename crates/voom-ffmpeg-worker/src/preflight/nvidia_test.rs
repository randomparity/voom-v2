use std::path::{Path, PathBuf};

use super::*;

#[test]
fn nvidia_uuid_validation_rejects_ordinals_and_partial_tokens() {
    assert!(validate_nvidia_uuid("GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").is_ok());
    for invalid in ["0", "GPU-0", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"] {
        assert!(validate_nvidia_uuid(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn nvidia_ffmpeg_commands_bind_the_configured_uuid_centrally() {
    let config = NvidiaPreflightConfig {
        device_uuid: "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        max_sessions: 1,
        nvidia_smi_path: PathBuf::from("nvidia-smi"),
    };

    let command = nvidia_ffmpeg_command(Path::new("ffmpeg"), &config);
    let cuda_binding = command
        .get_envs()
        .find(|(name, _)| *name == "CUDA_VISIBLE_DEVICES")
        .and_then(|(_, value)| value)
        .map(|value| value.to_string_lossy().into_owned());

    assert_eq!(cuda_binding.as_deref(), Some(config.device_uuid.as_str()));
}

#[cfg(unix)]
#[test]
fn nvidia_preflight_runs_base_checks_before_interpreting_device_identity() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("commands.log");
    let ffmpeg = stub_bin(
        temp.path(),
        "ffmpeg",
        &format!(
            "#!/bin/sh\nprintf 'ffmpeg:%s\\n' \"$*\" >> '{}'\n\
             case \"$*\" in\n\
               *-version*) echo 'ffmpeg version 7.0' ;;\n\
               *-encoders*) cat <<'EOF'\n{ALL_ENCODERS}EOF\n    ;;\n\
               *-muxers*) cat <<'EOF'\n{ALL_MUXERS}EOF\n    ;;\n\
               *) exit 2 ;;\n\
             esac\n",
            log.display()
        ),
    );
    let ffprobe = stub_bin(
        temp.path(),
        "ffprobe",
        &format!(
            "#!/bin/sh\nprintf 'ffprobe:%s\\n' \"$*\" >> '{}'\necho 'ffprobe version 7.0'\n",
            log.display()
        ),
    );
    let nvidia_smi = stub_bin(
        temp.path(),
        "nvidia-smi",
        &format!(
            "#!/bin/sh\nprintf 'nvidia-smi:%s\\n' \"$*\" >> '{}'\necho 'malformed identity'\n",
            log.display()
        ),
    );
    let config = NvidiaPreflightConfig {
        device_uuid: "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        max_sessions: 1,
        nvidia_smi_path: nvidia_smi,
    };

    let error = preflight_with_nvidia(&ffmpeg, &ffprobe, &config)
        .unwrap_err()
        .to_string();
    let commands = std::fs::read_to_string(log).unwrap();

    assert!(
        error.contains("nvidia-smi returned unexpected identity `malformed identity`"),
        "the identity response must retain its distinct interpretation: {error}"
    );
    assert_eq!(
        commands.lines().collect::<Vec<_>>(),
        [
            "ffmpeg:-hide_banner -version",
            "ffprobe:-hide_banner -version",
            "ffmpeg:-hide_banner -encoders",
            "ffmpeg:-hide_banner -muxers",
            "nvidia-smi:-i GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee \
             --query-gpu=uuid,name,driver_version --format=csv,noheader,nounits",
        ],
        "base FFmpeg capability checks must complete before NVIDIA identity interpretation"
    );
}

#[cfg(unix)]
#[test]
fn nvidia_preflight_runs_the_device_bound_stages_in_order() {
    const CAPACITY_STAGE: &str =
        "ffmpeg:-hide_banner -nostdin -f lavfi -re -i testsrc2=size=256x256:rate=30 -t 1";

    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("nvidia-stages.log");
    let pid = temp.path().join("identity.pid");
    let ffmpeg = nvidia_ffmpeg_stage_stub(temp.path(), &log, &pid);
    let ffprobe = fake_ffprobe(temp.path());
    let nvidia_smi = nvidia_smi_stage_stub(temp.path(), &log, &pid);
    let config = NvidiaPreflightConfig {
        device_uuid: "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        max_sessions: 1,
        nvidia_smi_path: nvidia_smi,
    };

    let nvidia = preflight_with_nvidia(&ffmpeg, &ffprobe, &config)
        .unwrap()
        .nvidia
        .unwrap();
    let commands = std::fs::read_to_string(log).unwrap();
    let stages = [
        "ffmpeg:-hide_banner -version",
        "ffmpeg:-hide_banner -encoders",
        "ffmpeg:-hide_banner -muxers",
        "nvidia-smi:-i",
        "ffmpeg:-hide_banner -encoders",
        "ffmpeg:-hide_banner -filters",
        "ffmpeg:-hide_banner -nostdin -f lavfi -re",
        "nvidia-smi:--query-compute-apps",
        "ffmpeg:-hide_banner -nostdin -f lavfi -i",
        "ffmpeg:-hide_banner -nostdin -f lavfi -i",
        "ffmpeg:-hide_banner -nostdin -hwaccel cuda",
        CAPACITY_STAGE,
    ];
    let mut remainder = commands.as_str();
    for stage in stages {
        let index = remainder
            .find(stage)
            .unwrap_or_else(|| panic!("missing ordered NVIDIA stage `{stage}` in {commands}"));
        remainder = &remainder[index + stage.len()..];
    }
    assert!(
        commands
            .lines()
            .last()
            .is_some_and(|line| line.contains(CAPACITY_STAGE)),
        "the capacity probe must be the final NVIDIA preflight stage: {commands}"
    );

    assert_eq!(nvidia.device_uuid, config.device_uuid);
    assert_eq!(nvidia.decoders, ["h264_cuvid", "hevc_cuvid", "av1_cuvid"]);
    assert!(nvidia.decoder_diagnostics.is_empty());
}

#[cfg(unix)]
fn nvidia_ffmpeg_stage_stub(dir: &Path, log: &Path, pid: &Path) -> PathBuf {
    let encoders = format!("{ALL_ENCODERS} V..... hevc_nvenc NVIDIA HEVC\n");
    let body = format!(
        "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\n\
         printf 'ffmpeg:%s\\n' \"$*\" >> '{log}'\n\
         case \"$*\" in\n\
           *-version*) echo 'ffmpeg version 7.0' ;;\n\
           *-encoders*) cat <<'EOF'\n{encoders}EOF\n    ;;\n\
           *-muxers*) cat <<'EOF'\n{ALL_MUXERS}EOF\n    ;;\n\
           *-filters*) echo '... hwupload_cuda'; echo '... scale_cuda' ;;\n\
           *hevc_nvenc*) test \"$CUDA_VISIBLE_DEVICES\" = \
             'GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee' || exit 9; \
             case \"$*\" in *'-t 3'*) echo $$ > '{pid}'; sleep 1 ;; esac ;;\n\
           *'-hwaccel cuda'*) test \"$CUDA_VISIBLE_DEVICES\" = \
             'GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee' || exit 9 ;;\n\
           *'-c:v libx264'*|*'-c:v libx265'*|*'-c:v libsvtav1'*) : > \"$last\" ;;\n\
           *) exit 2 ;;\n\
         esac\n",
        log = log.display(),
        pid = pid.display(),
    );
    stub_bin(dir, "ffmpeg", &body)
}

#[cfg(unix)]
fn nvidia_smi_stage_stub(dir: &Path, log: &Path, pid: &Path) -> PathBuf {
    let body = format!(
        "#!/bin/sh\nprintf 'nvidia-smi:%s\\n' \"$*\" >> '{log}'\n\
         case \"$*\" in\n\
           *--query-gpu*) echo \
             'GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee, Test GPU, 555.1' ;;\n\
           *--query-compute-apps*) if test -s '{pid}'; then printf '%s, %s\\n' \
             \"$(cat '{pid}')\" 'GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee'; fi ;;\n\
           *) exit 2 ;;\n\
         esac\n",
        log = log.display(),
        pid = pid.display(),
    );
    stub_bin(dir, "nvidia-smi", &body)
}

#[cfg(unix)]
const ALL_ENCODERS: &str = concat!(
    "Encoders:\n",
    " V..... libx265 H.265 / HEVC\n",
    " V..... libsvtav1 SVT-AV1\n",
    " V..... libaom-av1 libaom AV1\n",
    " A..... aac AAC\n",
    " A..... libopus Opus\n",
);
#[cfg(unix)]
const ALL_MUXERS: &str = "Muxers:\n E matroska Matroska\n E mp4 MP4\n E ogg Ogg\n";

#[cfg(unix)]
fn fake_ffprobe(dir: &Path) -> PathBuf {
    stub_bin(dir, "ffprobe", "#!/bin/sh\necho 'ffprobe version 7.0'\n")
}

#[cfg(unix)]
fn stub_bin(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    make_executable(&path);
    path
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}
