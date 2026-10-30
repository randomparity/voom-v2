#![expect(
    clippy::unwrap_used,
    reason = "generated-media assertions fail loudly with fixture context"
)]

#[path = "support/primary_audio_media.rs"]
mod media;

use std::path::PathBuf;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use tokio::sync::oneshot;
use voom_control_plane::policy::{
    ComplianceExecuteData, ComplianceExecutionOptions, PolicyInputFromScanInput,
    RootScopedScanInput,
};
use voom_control_plane::workers::RegisterNodeInput;
use voom_control_plane::{ControlPlane, HealthPlane};
use voom_core::{ArtifactAccessMode, FileLocationId, NodeKind, OperationKind, PolicyInputSetId};
use voom_node_agent::config::{
    AgentConfig, LoadedAgentConfig, StorageRootBinding, TokenSource, WorkerConfig,
};
use voom_node_agent::runtime::AgentRuntime;
use voom_store::repo::media::identity::{FileLocationRepo, MediaSnapshotRepo};
use voom_test_support::TempDatabase;

const HANG_GUARD: Duration = Duration::from_mins(2);
const POLICY: &str =
    include_str!("../../voom-policy/fixtures/policies/audio-primary-compatibility.voom");

struct CompatibilityFixture {
    _database: TempDatabase,
    _root: tempfile::TempDir,
    root: PathBuf,
    cp: ControlPlane,
    pool: sqlx::SqlitePool,
    input_id: PolicyInputSetId,
    config: LoadedAgentConfig,
    root_id: voom_core::StorageRootId,
    url: String,
    stop: Option<oneshot::Sender<()>>,
    agent: Option<tokio::task::JoinHandle<Result<(), voom_core::VoomError>>>,
    server_stop: Option<oneshot::Sender<()>>,
    server: Option<tokio::task::JoinHandle<()>>,
}

fn owner_config(
    root: &std::path::Path,
    root_id: voom_core::StorageRootId,
    node_id: voom_core::NodeId,
    token: &str,
    address: std::net::SocketAddr,
) -> LoadedAgentConfig {
    let specs = [
        ("voom-ffmpeg-worker", vec![OperationKind::TranscodeAudio]),
        ("voom-mkvtoolnix-worker", vec![OperationKind::Remux]),
        ("voom-ffprobe-worker", vec![OperationKind::ProbeFile]),
        (
            "voom-verify-artifact-worker",
            vec![OperationKind::VerifyArtifact],
        ),
    ];
    let workers = specs
        .into_iter()
        .map(|(name, operations)| WorkerConfig {
            name: name.to_owned(),
            program: voom_test_support::worker::cargo_bin_or_build(name, name).unwrap(),
            args: Vec::new(),
            operations,
            artifact_access: vec![ArtifactAccessMode::SharedMount],
            dependencies: voom_node_agent::config::WorkerDependencyPaths {
                mkvmerge_bin: (name == "voom-mkvtoolnix-worker")
                    .then(|| media::dependency("mkvmerge")),
                ffmpeg_bin: (name == "voom-ffmpeg-worker").then(|| media::dependency("ffmpeg")),
                ffprobe_bin: matches!(name, "voom-ffmpeg-worker" | "voom-ffprobe-worker")
                    .then(|| media::dependency("ffprobe")),
                nvidia_smi_bin: None,
            },
            accelerator: None,
            max_parallel: 1,
        })
        .collect();
    LoadedAgentConfig {
        config: AgentConfig {
            control_plane_url: format!("http://{address}"),
            ca_cert: None,
            node_id,
            poll_interval_ms: 25,
            lease_ttl_seconds: 30,
            progress_idle_timeout_seconds: 20,
            shutdown_grace_seconds: 5,
            node_token: TokenSource::Env {
                name: "UNUSED_LOADED_COMPATIBILITY_TOKEN".to_owned(),
            },
            storage_roots: vec![StorageRootBinding {
                storage_root_id: root_id.0,
                provider_locator: root.to_path_buf(),
            }],
            probe_hash_reuse: false,
            workers,
        },
        node_token: SecretString::from(token.to_owned()),
    }
}

async fn wait_for_owner(
    pool: &sqlx::SqlitePool,
    node_id: voom_core::NodeId,
    agent: &mut tokio::task::JoinHandle<Result<(), voom_core::VoomError>>,
) {
    tokio::time::timeout(HANG_GUARD, async {
        loop {
            assert!(
                !agent.is_finished(),
                "owner agent exited before readiness: {:?}",
                (&mut *agent).await
            );
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM workers WHERE node_id = ? AND status = 'active'",
            )
            .bind(i64::try_from(node_id.0).unwrap())
            .fetch_one(pool)
            .await
            .unwrap();
            if count == 4 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
}

async fn start_fixture_api(
    url: &str,
    cp: &ControlPlane,
) -> (
    std::net::SocketAddr,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let health = HealthPlane::open(url).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (server_stop, shutdown) = oneshot::channel();
    let app = voom_api::router_with_control_plane(health, cp.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown.await;
            })
            .await
            .unwrap();
    });
    (address, server_stop, server)
}

impl CompatibilityFixture {
    async fn start(tracks: &[media::AudioFixtureTrack<'_>]) -> Self {
        let database = TempDatabase::new().unwrap();
        let url = voom_store::test_support::sqlite_url_for(database.path());
        voom_store::init(&url).await.unwrap();
        let cp = ControlPlane::open(&url).await.unwrap();
        let pool = voom_store::connect(&url).await.unwrap();
        let root_guard = tempfile::tempdir().unwrap();
        let root = root_guard.path().to_path_buf();
        let source = media::generate_fixture(&root, tracks);
        let normalized = voom_ffprobe_worker::normalize_ffprobe_json(
            media::probe(&source),
            "fixture-actual",
            "2026-10-28T00:00:00Z",
        )
        .unwrap();
        let root_id = voom_store::test_support::seed_test_storage_root(&pool)
            .await
            .unwrap();
        voom_store::test_support::set_test_storage_root_path(&pool, &root)
            .await
            .unwrap();
        voom_store::test_support::set_test_storage_root_self_defaults(&pool)
            .await
            .unwrap();
        let registered = cp
            .register_node(RegisterNodeInput {
                name: "compatibility-owner".to_owned(),
                kind: NodeKind::Remote,
                heartbeat_ttl_seconds: 600,
                metadata: json!({}),
            })
            .await
            .unwrap();
        // Existing workflow promotion requires the colocated process namespace
        // to identify the same logical owner; workers still run through the agent.
        let cp = cp.with_local_node_id(Some(registered.node.id));
        sqlx::query("UPDATE library_roots SET owner_node_id = ? WHERE id = ?")
            .bind(i64::try_from(registered.node.id.0).unwrap())
            .bind(i64::try_from(root_id.0).unwrap())
            .execute(&pool)
            .await
            .unwrap();
        voom_test_support::scan_seed::seed_scanned_files(
            &cp,
            &url,
            root_id,
            &[voom_test_support::scan_seed::SeedFile {
                locator: "source.mkv",
                path: &source,
                probe_snapshot: normalized,
                sidecars: Vec::new(),
            }],
        )
        .await
        .unwrap();
        let (address, server_stop, server) = start_fixture_api(&url, &cp).await;
        let config = owner_config(
            &root,
            root_id,
            registered.node.id,
            registered.token.expose_secret(),
            address,
        );
        let (stop, shutdown) = oneshot::channel();
        let mut agent = tokio::spawn(AgentRuntime::new(config.clone()).unwrap().run_until(
            async move {
                let _ = shutdown.await;
            },
        ));
        wait_for_owner(&pool, registered.node.id, &mut agent).await;
        let input_id = cp
            .create_policy_input_set_from_root(RootScopedScanInput {
                slug: "compatibility-input".to_owned(),
                library_root_id: root_id,
            })
            .await
            .unwrap()
            .input_set_id;
        Self {
            _database: database,
            _root: root_guard,
            root,
            cp,
            pool,
            input_id,
            config,
            root_id,
            url,
            stop: Some(stop),
            agent: Some(agent),
            server_stop: Some(server_stop),
            server: Some(server),
        }
    }

    async fn execute(&self, source: &str, slug: &str) -> ComplianceExecuteData {
        let source = source.replace("primary audio compatibility", slug);
        let policy = self.cp.create_policy_document(slug, &source).await.unwrap();
        self.execute_version(policy.version.id).await
    }

    async fn execute_version(&self, version: voom_core::PolicyVersionId) -> ComplianceExecuteData {
        let options = ComplianceExecutionOptions {
            audio_staging_root: self.root.join("audio-stage"),
            audio_target_dir: self.root.join("audio-out"),
            remux_staging_root: self.root.join("remux-stage"),
            remux_target_dir: self.root.join("remux-out"),
            ..ComplianceExecutionOptions::default()
        };
        tokio::time::timeout(
            HANG_GUARD,
            self.cp
                .execute_compliance_policy_with_options(version, self.input_id, options),
        )
        .await
        .unwrap()
        .unwrap()
    }

    async fn pause_owner(&mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        tokio::time::timeout(HANG_GUARD, self.agent.take().unwrap())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    async fn resume_owner(&mut self) {
        let (stop, shutdown) = oneshot::channel();
        self.agent = Some(tokio::spawn(
            AgentRuntime::new(self.config.clone())
                .unwrap()
                .run_until(async move {
                    let _ = shutdown.await;
                }),
        ));
        self.stop = Some(stop);
        wait_for_owner(
            &self.pool,
            self.config.config.node_id,
            self.agent.as_mut().unwrap(),
        )
        .await;
    }

    async fn reseed_output(&mut self, path: &std::path::Path, slug: &str) {
        assert!(
            self.agent.is_none(),
            "scan fixture changes ownership only while the real owner is stopped"
        );
        let normalized = voom_ffprobe_worker::normalize_ffprobe_json(
            media::probe(path),
            "fixture-reprobe",
            "2026-10-28T00:00:00Z",
        )
        .unwrap();
        let locator = path.strip_prefix(&self.root).unwrap().to_str().unwrap();
        let seeded = voom_test_support::scan_seed::seed_scanned_files(
            &self.cp,
            &self.url,
            self.root_id,
            &[voom_test_support::scan_seed::SeedFile {
                locator,
                path,
                probe_snapshot: normalized,
                sidecars: Vec::new(),
            }],
        )
        .await
        .unwrap();
        self.resume_owner().await;
        self.input_id = self
            .cp
            .create_policy_input_set_from_scan(PolicyInputFromScanInput {
                slug: slug.to_owned(),
                file_version_id: seeded[0].file_version_id,
                media_snapshot_id: seeded[0].media_snapshot_id,
                container: "mkv".to_owned(),
                video_codec: "ffv1".to_owned(),
            })
            .await
            .unwrap()
            .input_set_id;
    }

    async fn final_path(&self, result: &ComplianceExecuteData) -> PathBuf {
        assert_eq!(result.summary.failure_count, 0, "{result:?}");
        let id = result
            .file_phases
            .iter()
            .rev()
            .find_map(|phase| phase.produced_file_location_id);
        assert!(id.is_some(), "no produced location: {result:?}");
        let id = id.unwrap();
        let location = self
            .cp
            .identity()
            .get_file_location(FileLocationId(id))
            .await
            .unwrap()
            .unwrap();
        let (_, relative) = location.rooted_address().unwrap();
        self.root.join(relative.as_str())
    }

    async fn shutdown(mut self) {
        self.pause_owner().await;
        self.server_stop.take().unwrap().send(()).unwrap();
        tokio::time::timeout(HANG_GUARD, self.server.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
}

impl Drop for CompatibilityFixture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(agent) = self.agent.take() {
            agent.abort();
        }
        if let Some(stop) = self.server_stop.take() {
            let _ = stop.send(());
        }
        if let Some(server) = self.server.take() {
            server.abort();
        }
    }
}

const RANKING_TRACKS: [media::AudioFixtureTrack<'static>; 5] = [
    media::AudioFixtureTrack {
        title: "Unknown",
        language: None,
        channels: 2,
        layout: "stereo",
        frequency: 330,
        commentary: false,
        codec: "wavpack",
    },
    media::AudioFixtureTrack {
        title: "Commentary",
        language: Some("eng"),
        channels: 2,
        layout: "stereo",
        frequency: 440,
        commentary: true,
        codec: "wavpack",
    },
    media::AudioFixtureTrack {
        title: "Primary",
        language: Some("eng"),
        channels: 2,
        layout: "stereo",
        frequency: 880,
        commentary: false,
        codec: "wavpack",
    },
    media::AudioFixtureTrack {
        title: "Secondary",
        language: Some("eng"),
        channels: 2,
        layout: "stereo",
        frequency: 1100,
        commentary: false,
        codec: "wavpack",
    },
    media::AudioFixtureTrack {
        title: "Foreign",
        language: Some("fra"),
        channels: 2,
        layout: "stereo",
        frequency: 660,
        commentary: false,
        codec: "wavpack",
    },
];

#[tokio::test]
async fn primary_audio_compatibility_executes_on_owner() {
    let fixture = CompatibilityFixture::start(&RANKING_TRACKS).await;
    let initial_probe = media::probe(&fixture.root.join("source.mkv"));
    assert_eq!(initial_probe["streams"][1]["codec_type"], "audio");
    assert_eq!(initial_probe["streams"][2]["codec_type"], "subtitle");
    assert_eq!(initial_probe["streams"][3]["codec_type"], "audio");
    for index in [0, 2] {
        assert_eq!(initial_probe["streams"][index]["disposition"]["default"], 1);
        assert_eq!(initial_probe["streams"][index]["disposition"]["forced"], 1);
    }
    let non_audio = media::non_audio_state(&initial_probe);
    let original = media::packet_hashes(&fixture.root.join("source.mkv"));
    let result = fixture.execute(POLICY, "compatibility-full").await;
    let output = fixture.final_path(&result).await;
    let actual = media::probe(&output);
    let audio = actual["streams"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .collect::<Vec<_>>();
    assert_eq!(
        audio.len(),
        5,
        "four retained originals plus one companion; foreign audio removed"
    );
    assert_eq!(audio[0]["codec_name"], "aac");
    assert_eq!(audio[0]["disposition"]["default"], 1);
    assert!(audio[1..].iter().all(|s| s["disposition"]["default"] == 0));
    assert_eq!(media::tag(audio[0], "title").as_deref(), Some("Primary"));
    media::assert_selected_tone(
        &output,
        audio[0]["index"].as_u64().unwrap(),
        880,
        &[330, 440, 660, 1100],
    );
    assert!(
        audio
            .iter()
            .any(|s| media::tag(s, "title").as_deref() == Some("Commentary")
                && s["disposition"]["comment"] == 1)
    );
    let retained = media::packet_hashes(&output);
    for (name, packets) in &original {
        if name.starts_with("Foreign|") {
            assert!(
                !retained.contains_key(name),
                "foreign-language original removed"
            );
        } else {
            assert_eq!(retained.get(name), Some(packets), "retained {name}");
        }
    }
    assert_eq!(synthesis_state(&fixture).await, (1, 1, 1, 1));
    media::assert_attachment(&output, &fixture.root);
    assert_eq!(
        actual["streams"][0]["codec_type"], "video",
        "audio placement cannot take the video slot"
    );
    assert_eq!(
        media::non_audio_state(&actual),
        non_audio,
        "non-audio slots, order and dispositions survive committed placement"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn compatibility_generated_target_matrix() {
    for (channels, layout, expected_layout) in [
        (1, "mono", "mono"),
        (2, "stereo", "stereo"),
        (3, "2.1", "2.1"),
        (3, "3.0", "3.0"),
        (3, "3.0(back)", "3.0(back)"),
        (4, "3.1", "3.1"),
        (4, "4.0", "4.0"),
        (4, "quad", "quad(side)"),
        (4, "quad(side)", "quad(side)"),
        (4, "4 channels (FL+FR+LFE+BC)", "4 channels (FL+FR+LFE+BC)"),
        (5, "4.1", "4.1"),
        (5, "5.0", "5.0(side)"),
        (5, "5.0(side)", "5.0(side)"),
        (6, "5.1", "5.1(side)"),
        (6, "5.1(side)", "5.1(side)"),
        (8, "7.1", "5.1(side)"),
    ] {
        let tracks = [media::AudioFixtureTrack {
            title: "Source",
            language: None,
            channels,
            layout,
            frequency: 880,
            commentary: false,
            codec: "wavpack",
        }];
        let fixture = CompatibilityFixture::start(&tracks).await;
        let original = media::packet_hashes(&fixture.root.join("source.mkv"));
        let result = fixture.execute(POLICY, "compatibility-matrix").await;
        let output = fixture.final_path(&result).await;
        let actual = media::probe(&output);
        let audio = actual["streams"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["codec_type"] == "audio")
            .collect::<Vec<_>>();
        assert_eq!(
            audio.len(),
            2,
            "one unknown original and its compatibility companion"
        );
        assert_eq!(
            audio[0]["codec_name"],
            if channels <= 2 { "aac" } else { "eac3" }
        );
        assert_eq!(
            audio[0]["channels"],
            if channels == 8 { 6 } else { channels }
        );
        assert_eq!(audio[0]["channel_layout"], expected_layout);
        assert_eq!(audio[0]["disposition"]["default"], 1);
        assert_eq!(audio[1]["disposition"]["default"], 0);
        let retained = media::packet_hashes(&output);
        for (name, packets) in original {
            assert_eq!(
                retained.get(&name),
                Some(&packets),
                "retained {channels} {layout} {name}"
            );
        }
        fixture.shutdown().await;
    }
    compatible_originals_do_not_encode().await;
}

async fn compatible_originals_do_not_encode() {
    for (codec, channels, layout) in [
        ("aac", 1, "mono"),
        ("aac", 2, "stereo"),
        ("eac3", 6, "5.1(side)"),
    ] {
        let tracks = [
            media::AudioFixtureTrack {
                title: "Unknown",
                language: None,
                channels: 2,
                layout: "stereo",
                frequency: 330,
                commentary: false,
                codec: "wavpack",
            },
            media::AudioFixtureTrack {
                title: "Compatible",
                language: Some("eng"),
                channels,
                layout,
                frequency: 880,
                commentary: false,
                codec,
            },
        ];
        let fixture = CompatibilityFixture::start(&tracks).await;
        let original = media::packet_hashes(&fixture.root.join("source.mkv"));
        let result = fixture.execute(POLICY, "compatibility-existing").await;
        let output = fixture.final_path(&result).await;
        let actual = media::probe(&output);
        let audio = actual["streams"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["codec_type"] == "audio")
            .collect::<Vec<_>>();
        assert_eq!(
            audio.len(),
            2,
            "compatible ranked originals require no companion"
        );
        assert_eq!(media::tag(audio[0], "title").as_deref(), Some("Compatible"));
        assert_eq!(audio[0]["codec_name"], codec);
        assert_eq!(audio[0]["channels"], channels);
        assert_eq!(audio[0]["disposition"]["default"], 1);
        assert_eq!(audio[1]["disposition"]["default"], 0);
        assert_eq!(
            media::packet_hashes(&output),
            original,
            "compatible original packets must never be encoded again"
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audio_synthesis_operations")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        fixture.shutdown().await;
    }
}

fn ensure_only_policy() -> String {
    format!(
        "{}}}\n",
        POLICY.split("  phase placement {").next().unwrap()
    )
}

fn recovery_tracks() -> [media::AudioFixtureTrack<'static>; 2] {
    [
        media::AudioFixtureTrack {
            title: "Unknown",
            language: None,
            channels: 2,
            layout: "stereo",
            frequency: 330,
            commentary: false,
            codec: "wavpack",
        },
        media::AudioFixtureTrack {
            title: "Primary",
            language: Some("eng"),
            channels: 2,
            layout: "stereo",
            frequency: 880,
            commentary: false,
            codec: "wavpack",
        },
    ]
}

async fn synthesis_state(fixture: &CompatibilityFixture) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT COUNT(*) FROM audio_synthesis_operations), (SELECT COUNT(*) FROM audio_synthesis_companions), (SELECT COUNT(*) FROM audio_synthesis_stream_lineage), (SELECT COUNT(*) FROM audio_synthesis_dispatch_attempts)")
        .fetch_one(&fixture.pool).await.unwrap()
}

#[tokio::test]
async fn compatibility_recovers_between_ensure_and_place() {
    let mut fixture = CompatibilityFixture::start(&recovery_tracks()).await;
    let ensured = fixture
        .execute(&ensure_only_policy(), "compatibility-ensure-only")
        .await;
    let ensured_path = fixture.final_path(&ensured).await;
    let before = synthesis_state(&fixture).await;
    assert_eq!(before, (1, 1, 1, 1));
    let ensured_probe = media::probe(&ensured_path);
    let placement_non_audio = media::non_audio_state(&ensured_probe);
    let derived = ensured_probe["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|stream| {
            media::tag(stream, "voom_compatibility_source_stream_id")
                .map(|_| media::tag(stream, "voom_compatibility_stream_id").unwrap())
        })
        .unwrap();
    fixture.pause_owner().await;
    fixture
        .reseed_output(&ensured_path, "ensured-reprobe")
        .await;
    let placed = fixture.execute(POLICY, "compatibility-resume").await;
    let output = fixture.final_path(&placed).await;
    let actual = media::probe(&output);
    assert_eq!(
        media::non_audio_state(&actual),
        placement_non_audio,
        "actual input to placement and committed output retain non-audio slots and dispositions"
    );
    let audio = actual["streams"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .collect::<Vec<_>>();
    assert_eq!(audio.len(), 3);
    assert_eq!(
        media::tag(audio[0], "voom_compatibility_stream_id").as_deref(),
        Some(derived.as_str())
    );
    assert_eq!(audio[0]["disposition"]["default"], 1);
    assert!(
        audio[1..]
            .iter()
            .all(|stream| stream["disposition"]["default"] == 0)
    );
    assert_eq!(synthesis_state(&fixture).await, before);
    fixture.pause_owner().await;
    fixture.reseed_output(&output, "placed-reprobe").await;
    let reapplied = fixture.execute(POLICY, "compatibility-reapply").await;
    assert_eq!(reapplied.summary.failure_count, 0, "{reapplied:?}");
    assert!(
        reapplied
            .file_phases
            .iter()
            .filter(|phase| phase.phase_ordinal < 3)
            .all(|phase| phase.produced_file_version_id.is_none()),
        "already placed media must skip retention, encoding, and placement: {reapplied:?}"
    );
    assert_eq!(
        fixture.final_path(&reapplied).await,
        output,
        "verification reports the existing final file"
    );
    assert_eq!(synthesis_state(&fixture).await, before);
    fixture.shutdown().await;
}

#[tokio::test]
async fn compatibility_retry_reuses_committed_result() {
    let fixture = CompatibilityFixture::start(&recovery_tracks()).await;
    let first = fixture
        .execute(&ensure_only_policy(), "compatibility-canonical-retry")
        .await;
    let first_path = fixture.final_path(&first).await;
    let before = synthesis_state(&fixture).await;
    let tickets = voom_store::repo::execution::tickets::SqliteTicketRepo::new(fixture.pool.clone());
    let first_ticket = first
        .file_phases
        .iter()
        .flat_map(|phase| &phase.ticket_ids)
        .next()
        .unwrap();
    let original = tickets
        .get(voom_core::TicketId(*first_ticket))
        .await
        .unwrap()
        .unwrap();
    let now = fixture.cp.clock().now();
    let job = fixture
        .cp
        .open_job(voom_store::repo::execution::jobs::NewJob {
            kind: "compatibility-canonical-retry".to_owned(),
            priority: 0,
            created_at: now,
        })
        .await
        .unwrap();
    let retry = fixture
        .cp
        .create_ticket(voom_store::repo::execution::tickets::NewTicket {
            job_id: Some(job.id),
            kind: original.kind,
            priority: 0,
            payload: original.payload,
            max_attempts: 1,
            created_at: now,
        })
        .await
        .unwrap();
    fixture
        .cp
        .mark_ready_if_unblocked(retry.id, now)
        .await
        .unwrap();
    let settled = tokio::time::timeout(HANG_GUARD, async {
        loop {
            let ticket = tickets.get(retry.id).await.unwrap().unwrap();
            if matches!(
                ticket.state,
                voom_store::repo::execution::tickets::TicketState::Succeeded
                    | voom_store::repo::execution::tickets::TicketState::Failed
            ) {
                break ticket;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        settled.state,
        voom_store::repo::execution::tickets::TicketState::Succeeded,
        "{settled:?}"
    );
    let reused = settled.result.unwrap();
    let location = fixture
        .cp
        .identity()
        .get_file_location(FileLocationId(
            reused["result_file_location_id"].as_u64().unwrap(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture
            .root
            .join(location.rooted_address().unwrap().1.as_str()),
        first_path
    );
    assert_eq!(
        synthesis_state(&fixture).await,
        before,
        "same admitted operation must reuse its canonical commit, not encode or append lineage"
    );
    assert_exact_generation(&fixture).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn compatibility_blocked_source_does_not_mutate_media() {
    for (channels, layout, commentary, code) in [
        (
            7,
            "6.1",
            false,
            "unsupported compatibility audio layout 6.1 with 7 channels",
        ),
        (
            2,
            "stereo",
            true,
            "requires an English or unknown-language non-commentary original source",
        ),
    ] {
        let fixture = CompatibilityFixture::start(&[media::AudioFixtureTrack {
            title: "Ineligible",
            language: Some("eng"),
            channels,
            layout,
            frequency: 440,
            commentary,
            codec: "wavpack",
        }])
        .await;
        let original = media::packet_hashes(&fixture.root.join("source.mkv"));
        let policy = fixture
            .cp
            .create_policy_document("primary-audio-compatibility", POLICY)
            .await
            .unwrap();
        let result = fixture
            .cp
            .execute_compliance_policy_with_options(
                policy.version.id,
                fixture.input_id,
                ComplianceExecutionOptions::default(),
            )
            .await;
        let data = match result {
            Ok(data) => data,
            Err(error) => {
                assert!(
                    error.partial.is_some(),
                    "blocked workflow lost diagnostics: {:?}",
                    error.source
                );
                error.partial.unwrap()
            }
        };
        let reports = serde_json::to_value(&data.phases).unwrap();
        assert!(
            reports.to_string().contains(code),
            "missing {code}: {data:?}"
        );
        assert!(
            data.file_phases
                .iter()
                .all(|phase| phase.produced_file_version_id.is_none()),
            "blocked ensure cannot publish a mutation: {data:?}"
        );
        assert_eq!(synthesis_state(&fixture).await, (0, 0, 0, 0));
        assert_eq!(
            media::packet_hashes(&fixture.root.join("source.mkv")),
            original
        );
        fixture.shutdown().await;
    }
}

async fn assert_exact_generation(fixture: &CompatibilityFixture) {
    let key: String = sqlx::query_scalar("SELECT operation_key FROM audio_synthesis_operations")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let repo =
        voom_store::repo::media::audio_synthesis_operations::SqliteAudioSynthesisOperationRepo::new(
            fixture.pool.clone(),
        );
    let record = repo.get_by_key(&key).await.unwrap().unwrap();
    let mut transaction =
        voom_store::tx::begin_read_only(&fixture.pool, "test: exact compatibility generation")
            .await
            .unwrap();
    let canonical = voom_store::repo::media::audio_synthesis_operations::SqliteAudioSynthesisOperationRepo::require_committed_lineage_in_tx(&mut transaction, &record).await.unwrap();
    let mut stale = record.clone();
    stale.operation.dispatch_generation += 1;
    let error = voom_store::repo::media::audio_synthesis_operations::SqliteAudioSynthesisOperationRepo::require_committed_lineage_in_tx(&mut transaction, &stale).await.unwrap_err();
    assert!(matches!(error, voom_core::VoomError::Database { .. }));
    assert_eq!(canonical.generation, record.operation.dispatch_generation);
    transaction.commit().await.unwrap();
}

#[tokio::test]
async fn ordinary_synthesis_preserves_compatibility_identities() {
    let tracks = [
        media::AudioFixtureTrack {
            title: "Unknown",
            language: None,
            channels: 2,
            layout: "stereo",
            frequency: 330,
            commentary: false,
            codec: "wavpack",
        },
        media::AudioFixtureTrack {
            title: "Primary",
            language: Some("eng"),
            channels: 6,
            layout: "5.1",
            frequency: 880,
            commentary: false,
            codec: "wavpack",
        },
    ];
    let mut fixture = CompatibilityFixture::start(&tracks).await;
    let compatible = fixture
        .execute(POLICY, "compatibility-before-ordinary")
        .await;
    let compatible_path = fixture.final_path(&compatible).await;
    let before = media::probe(&compatible_path);
    let associations = before["streams"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|stream| stream["codec_type"] == "audio")
        .map(|stream| {
            (
                media::tag(stream, "voom_compatibility_stream_id").unwrap(),
                media::tag(stream, "voom_compatibility_source_stream_id"),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let packets = media::packet_hashes(&compatible_path);
    let ordinary = fixture.execute("policy \"ordinary-after-compatibility\" { phase downmix { synthesize audio from channels >= 6 { codec aac channels 2 } } }", "ordinary-after-compatibility").await;
    let output = fixture.final_path(&ordinary).await;
    let actual = media::probe(&output);
    let snapshot_id = ordinary
        .file_phases
        .iter()
        .rev()
        .find_map(|phase| phase.reprobe_snapshot_id)
        .unwrap();
    let published = fixture
        .cp
        .identity()
        .get_media_snapshot(voom_core::MediaSnapshotId(snapshot_id))
        .await
        .unwrap()
        .unwrap();
    assert_published_ordinary_audio(&published, &actual, &associations);
    let after_packets = media::packet_hashes(&output);
    for (name, packet_hashes) in packets {
        assert_eq!(
            after_packets.get(&name),
            Some(&packet_hashes),
            "copied {name}"
        );
    }
    assert_eq!(synthesis_state(&fixture).await, (2, 3, 3, 2));
    fixture.pause_owner().await;
    fixture
        .reseed_output(&output, "ordinary-compatibility-reprobe")
        .await;
    let reapplied = fixture
        .execute(POLICY, "compatibility-after-ordinary")
        .await;
    assert_eq!(fixture.final_path(&reapplied).await, output);
    assert_eq!(
        synthesis_state(&fixture).await,
        (2, 3, 3, 2),
        "compatibility reapply cannot add another companion after ordinary synthesis"
    );
    fixture.shutdown().await;
}

fn assert_published_ordinary_audio(
    published: &voom_store::repo::media::identity::MediaSnapshot,
    actual: &serde_json::Value,
    associations: &std::collections::BTreeMap<String, Option<String>>,
) {
    let streams = actual["streams"].as_array().unwrap();
    let ids = published.payload["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|stream| stream["id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        ids.len(),
        streams.len(),
        "ordinary companions need distinct observed IDs before publication"
    );
    for stream in streams
        .iter()
        .filter(|stream| stream["codec_type"] == "audio")
    {
        let id = media::tag(stream, "voom_compatibility_stream_id").unwrap();
        assert!(
            ids.contains(id.as_str()),
            "actual audio identity must match the published owner probe"
        );
        let association = media::tag(stream, "voom_compatibility_source_stream_id");
        if let Some(original) = associations.get(&id) {
            assert_eq!(&association, original, "copied original association");
        } else {
            assert_eq!(stream["codec_name"], "aac");
            assert_eq!(stream["channels"], 2);
            assert_eq!(
                stream["disposition"]["default"], 0,
                "ordinary synthesis remains non-default"
            );
            assert!(
                association.is_none(),
                "ordinary companions cannot inherit compatibility ancestry"
            );
        }
    }
}
