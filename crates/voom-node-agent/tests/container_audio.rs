//! Installed media dependencies must cross the real owner's `env_clear` boundary.
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "generated-media assertions fail with test context"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use voom_core::{ArtifactAccessMode, LeaseId, OperationKind, WorkerId};
use voom_node_agent::child::{ChildSpec, ChildSupervisor, RunningChild};
use voom_node_agent::config::{WorkerConfig, WorkerDependencyPaths};
use voom_test_support::audio_samples::canonical_decoded_audio;
use voom_worker_protocol::{
    AUDIO_PROFILE_DEFAULT, AudioExpectedFacts, AudioStreamRef, ClientHandle, NdjsonOutcome,
    OperationRequest, ProbeFileRequest, ProbeFileResult, ProgressFrame, TranscodeAudioInput,
    TranscodeAudioOutput, TranscodeAudioRequest, TranscodeAudioResult, TranscodeAudioSelection,
    TranscodeAudioSettings, WorkerCredentials,
};

fn dependency(name: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap())
}

fn child_spec(name: &str, id: u64, operation: OperationKind) -> ChildSpec {
    ChildSpec::from_worker(
        &WorkerConfig {
            name: name.to_owned(),
            program: voom_test_support::worker::cargo_bin_or_build(name, name).unwrap(),
            args: Vec::new(),
            operations: vec![operation],
            artifact_access: vec![ArtifactAccessMode::SharedMount],
            dependencies: WorkerDependencyPaths {
                ffmpeg_bin: (operation == OperationKind::TranscodeAudio)
                    .then(|| dependency("VOOM_FFMPEG_BIN")),
                ffprobe_bin: Some(dependency("VOOM_FFPROBE_BIN")),
                ..WorkerDependencyPaths::default()
            },
            accelerator: None,
            max_parallel: 1,
        },
        WorkerCredentials {
            worker_id: WorkerId(id),
            worker_epoch: 1,
            secret: "generated-container-node-test".to_owned().into(),
        },
    )
}

async fn dispatch(
    child: &RunningChild,
    operation: OperationKind,
    payload: serde_json::Value,
    request_id: &str,
) -> serde_json::Value {
    let mut response = child
        .client()
        .dispatch(
            child.spec().credentials(),
            request_id,
            OperationRequest {
                operation,
                lease_id: LeaseId(763),
                payload,
                heartbeat_deadline_ms: 1000,
                progress_idle_deadline_ms: 30_000,
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_mins(1), async {
        loop {
            match response.frames.next_frame().await.unwrap() {
                NdjsonOutcome::Frame(_) => {}
                NdjsonOutcome::Terminated(ProgressFrame::Result { payload, .. }) => break payload,
                other => panic!("owner child operation failed: {other:?}"),
            }
        }
    })
    .await
    .unwrap()
}

fn write_wave(path: &Path, bits: u16) {
    let limit = 1_i32 << (bits - 1);
    let data: Vec<_> = [-limit, limit - 1, -1, 1, 0, 123, -123, 3]
        .into_iter()
        .cycle()
        .take(256)
        .flat_map(|sample| sample.to_le_bytes()[..usize::from(bits / 8)].to_vec())
        .collect();
    let mut format = Vec::new();
    for value in [0xfffe_u16, 2] {
        format.extend(value.to_le_bytes());
    }
    format.extend(48_000_u32.to_le_bytes());
    format.extend((48_000_u32 * 2 * u32::from(bits / 8)).to_le_bytes());
    for value in [2 * (bits / 8), bits, 22, bits] {
        format.extend(value.to_le_bytes());
    }
    format.extend(3_u32.to_le_bytes());
    format.extend([1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113]);
    let mut body = b"WAVEfmt ".to_vec();
    body.extend(40_u32.to_le_bytes());
    body.extend(format);
    body.extend(b"data");
    body.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
    body.extend(data);
    let mut wave = b"RIFF".to_vec();
    wave.extend(u32::try_from(body.len()).unwrap().to_le_bytes());
    wave.extend(body);
    std::fs::write(path, wave).unwrap();
}

async fn verify_pcm(children: &[RunningChild], directory: &Path, bits: u16) {
    let input = directory.join(format!("source-{bits}.wav"));
    let output = directory.join(format!("pcm-{bits}.mkv"));
    write_wave(&input, bits);
    let before = Box::pin(voom_ffprobe_worker::observe_file_facts(&input))
        .await
        .unwrap();
    let codec = format!("pcm_s{bits}le");
    let request = TranscodeAudioRequest {
        input: TranscodeAudioInput {
            path: input.to_string_lossy().into_owned(),
            expected: AudioExpectedFacts {
                size_bytes: before.size_bytes,
                content_hash: before.content_hash.clone(),
                modified_at: before.modified_at.clone(),
                local_file_key: None,
            },
        },
        output: TranscodeAudioOutput {
            staging_root: directory.to_string_lossy().into_owned(),
            path: output.to_string_lossy().into_owned(),
            container: "mkv".to_owned(),
            overwrite: false,
        },
        selection: TranscodeAudioSelection {
            selected_streams: vec![AudioStreamRef {
                snapshot_stream_id: "generated-integer-stereo".to_owned(),
                provider_stream_index: 0,
            }],
        },
        audio: TranscodeAudioSettings {
            target_codec: codec.clone(),
            profile: AUDIO_PROFILE_DEFAULT.to_owned(),
            add_track: false,
            target_channels: None,
            compatibility: None,
            lossless: None,
        },
    };
    let result: TranscodeAudioResult = serde_json::from_value(
        dispatch(
            &children[0],
            OperationKind::TranscodeAudio,
            serde_json::to_value(request).unwrap(),
            &format!("pcm-{bits}"),
        )
        .await,
    )
    .unwrap();
    assert_eq!(result.selected_output_streams[0].codec, codec);
    Box::pin(verify_probe_and_samples(
        &children[1],
        &input,
        &output,
        bits,
    ))
    .await;
    assert_eq!(
        before,
        Box::pin(voom_ffprobe_worker::observe_file_facts(&input))
            .await
            .unwrap()
    );
}

async fn verify_probe_and_samples(child: &RunningChild, input: &Path, output: &Path, bits: u16) {
    let observed = Box::pin(voom_ffprobe_worker::observe_file_facts(output))
        .await
        .unwrap();
    let request = ProbeFileRequest {
        path: output.to_string_lossy().into_owned(),
        expected: voom_worker_protocol::ExpectedFileFacts {
            size_bytes: observed.size_bytes,
            content_hash: observed.content_hash.clone(),
            modified_at: observed.modified_at.clone(),
            local_file_key: None,
        },
        guard: None,
    };
    let probe: ProbeFileResult = serde_json::from_value(
        dispatch(
            child,
            OperationKind::ProbeFile,
            serde_json::to_value(request).unwrap(),
            &format!("probe-{bits}"),
        )
        .await,
    )
    .unwrap();
    assert_eq!(probe.pre_probe, observed);
    assert_eq!(probe.post_probe, observed);
    assert_eq!(
        probe.snapshot["streams"][0]["codec_name"],
        format!("pcm_s{bits}le")
    );
    assert_eq!(probe.snapshot["streams"][0]["channel_layout"], "stereo");
    let ffmpeg = dependency("VOOM_FFMPEG_BIN");
    let ffprobe = dependency("VOOM_FFPROBE_BIN");
    let source = canonical_decoded_audio(&ffmpeg, &ffprobe, input, 0)
        .await
        .unwrap();
    let encoded = canonical_decoded_audio(&ffmpeg, &ffprobe, output, 0)
        .await
        .unwrap();
    assert_eq!(source.facts, encoded.facts);
    source.compare_samples(&encoded).unwrap();
}

#[tokio::test]
#[ignore = "requires installed PCM container tools; explicitly run by just container-worker-check"]
async fn installed_container_tools_cross_owner_child_environment() {
    let directory = tempfile::tempdir().unwrap();
    let supervisor = ChildSupervisor::new(Duration::from_secs(5), Duration::from_secs(5));
    let children = supervisor
        .start_all(vec![
            child_spec("voom-ffmpeg-worker", 763, OperationKind::TranscodeAudio),
            child_spec("voom-ffprobe-worker", 764, OperationKind::ProbeFile),
        ])
        .await
        .unwrap();
    for bits in [16, 24] {
        Box::pin(verify_pcm(&children, directory.path(), bits)).await;
    }
    supervisor.shutdown_all(children).await.unwrap();
}
