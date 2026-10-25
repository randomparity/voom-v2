use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Value, json};
use voom_control_plane::ControlPlane;
use voom_core::{FileVersionId, MediaSnapshotId, ScanObservationEvidence};
use voom_plan::planner::remux::{RemuxOperationPayload, resolve_track_keep_ids, stream_facts};

type SnapshotRow = (String, i64, i64, String, String);

pub async fn assert_cases(url: &str, observed: &Value, library: &Path) {
    let pool = voom_store::connect(url).await.unwrap();
    let rows = sqlx::query_as::<_, SnapshotRow>(
        "SELECT fl.provider_relative_locator, ms.file_version_id, ms.id, ms.payload, so.evidence_json \
         FROM media_snapshots ms JOIN file_locations fl ON fl.file_version_id = ms.file_version_id \
         JOIN scan_observations so ON so.provider_relative_locator = fl.provider_relative_locator \
         ORDER BY fl.provider_relative_locator",
    ).fetch_all(&pool).await.unwrap();
    assert_eq!(
        rows.len(),
        5,
        "each authored primary needs a durable snapshot"
    );
    pool.close().await;
    let cp = ControlPlane::open(url).await.unwrap();
    let policy = cp
        .create_policy_document(
            "chaos-track-selection",
            r#"
        policy "chaos-track-selection" { phase normalize {
            container mkv
            remove audio where commentary
            remove subtitle where forced
            defaults audio: first
            defaults subtitle: none
        } }
    "#,
        )
        .await
        .unwrap();
    let assets = observed["assets"].as_array().unwrap();
    assert_eq!(assets.len(), 5, "ASS stays a sidecar, not a primary asset");
    // Expected indices follow authored order: video, commentary-first audio, SRT.
    for (label, count, default, forced, kept) in [
        ("flags_multiple", 2, true, false, &[0, 2, 3, 4][..]),
        ("flags_none", 2, false, false, &[0, 2, 3, 4][..]),
        ("flags_default_forced", 2, true, true, &[0, 2][..]),
        (
            "flags_forced_only",
            8,
            false,
            true,
            &[0, 2, 3, 4, 5, 6, 7, 8][..],
        ),
        ("healthy", 1, true, false, &[0, 1, 2][..]),
    ] {
        let suffix = format!(" - {label}.mkv");
        let matches: Vec<_> = rows.iter().filter(|row| row.0.ends_with(&suffix)).collect();
        assert_eq!(matches.len(), 1, "exactly one snapshot for {label}");
        let (locator, version, snapshot_id, payload, evidence) = matches[0];
        let snapshot: Value = serde_json::from_str(payload).unwrap();
        let evidence: ScanObservationEvidence = serde_json::from_str(evidence).unwrap();
        evidence.validate().unwrap();
        assert_eq!(
            evidence.probe_snapshot["streams"].as_array().unwrap().len(),
            2 * count + 1
        );
        let asset = assets
            .iter()
            .find(|asset| asset["current_path"] == *locator)
            .unwrap();
        let subtitle_default = default && label != "healthy";
        assert_dispositions(
            &snapshot["streams"],
            count,
            default,
            subtitle_default,
            forced,
        );
        assert_dispositions(
            &asset["probed"]["streams"],
            count,
            default,
            subtitle_default,
            forced,
        );
        let audio: Vec<_> = snapshot["streams"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|stream| stream["kind"] == "audio")
            .collect();
        for (index, stream) in audio.iter().enumerate() {
            assert_eq!(
                stream["disposition"]["commentary"],
                index == 0 && label != "healthy"
            );
        }
        assert_sidecar(&evidence, asset, library, label == "healthy");
        assert_policy(
            &cp,
            policy.version.id,
            label,
            FileVersionId(u64::try_from(*version).unwrap()),
            MediaSnapshotId(u64::try_from(*snapshot_id).unwrap()),
            kept,
            &snapshot,
        )
        .await;
    }
}

fn assert_dispositions(
    streams: &Value,
    count: usize,
    audio_default: bool,
    subtitle_default: bool,
    forced: bool,
) {
    let streams = streams.as_array().unwrap();
    assert_eq!(
        streams.len(),
        2 * count + 1,
        "no sidecar is invented as an embedded stream"
    );
    for (kind, default) in [("audio", audio_default), ("subtitle", subtitle_default)] {
        let selected: Vec<_> = streams
            .iter()
            .filter(|stream| stream["kind"] == kind)
            .collect();
        assert_eq!(selected.len(), count, "{kind} count");
        for stream in selected {
            // Exported flags are flat; persisted normalized flags are nested.
            let flags = stream.get("disposition").unwrap_or(stream);
            assert_eq!(
                flags["default"].as_bool(),
                Some(default),
                "{kind} default evidence"
            );
            assert_eq!(
                flags["forced"].as_bool(),
                Some(forced),
                "{kind} forced evidence"
            );
        }
    }
}

fn assert_sidecar(
    evidence: &ScanObservationEvidence,
    asset: &Value,
    library: &Path,
    healthy: bool,
) {
    if healthy {
        assert_eq!(evidence.sidecars.len(), 1);
        let sidecar = &evidence.sidecars[0];
        assert_eq!(sidecar.role, "external_subtitle");
        assert!(sidecar.provider_relative_locator.ends_with(".fra.ass"));
        let bytes = std::fs::read(library.join(&sidecar.provider_relative_locator)).unwrap();
        assert_eq!(sidecar.size_bytes, u64::try_from(bytes.len()).unwrap());
        assert_eq!(sidecar.blake3_hex, blake3::hash(&bytes).to_hex().as_str());
        let exported = asset["sidecars"].as_array().unwrap();
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0]["kind"], "subtitle");
        assert_eq!(exported[0]["path"], sidecar.provider_relative_locator);
    } else {
        assert!(evidence.sidecars.is_empty());
        assert!(asset.get("sidecars").is_none());
    }
}

async fn assert_policy(
    cp: &ControlPlane,
    policy: voom_core::PolicyVersionId,
    label: &str,
    version: FileVersionId,
    snapshot: MediaSnapshotId,
    kept: &[u32],
    source: &Value,
) {
    // Bootstrap has already shut down the owner. Create a durable offline input
    // from its real snapshot; do not manufacture availability to use scan import.
    let input = voom_policy::MediaSnapshotInput {
        ordinal: 1,
        target: voom_policy::TargetRef::FileVersion { id: version },
        container: source["container"]["format_name"]
            .as_str()
            .map(str::to_owned),
        stream_summary: json!({"streams": source["streams"]}),
        video_codec: None,
        width: None,
        height: None,
        hdr: None,
        bitrate: None,
        duration_millis: None,
        audio_languages: Vec::new(),
        subtitle_languages: Vec::new(),
        health_flags: Vec::new(),
        existing_media_snapshot_id: Some(snapshot),
    };
    let created = cp
        .create_policy_input_set(voom_policy::PolicyInputSetDraft {
            slug: label.to_owned(),
            display_name: label.to_owned(),
            schema_version: 1,
            source_kind: voom_policy::PolicyInputSourceKind::Imported,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            description: None,
            fixture_labels: vec![format!("chaos-track-{label}")],
            synthetic_targets: Vec::new(),
            media_snapshots: vec![input.clone()],
            identity_evidence: Vec::new(),
            bundle_targets: Vec::new(),
            quality_profiles: Vec::new(),
            issues: Vec::new(),
        })
        .await
        .unwrap();
    let plan = cp
        .plan_accepted_policy_version_with_input_set(policy, created.id)
        .await
        .unwrap();
    let repeat = cp
        .plan_accepted_policy_version_with_input_set(policy, created.id)
        .await
        .unwrap();
    assert_eq!(plan.plan_hash, repeat.plan_hash);
    assert_eq!(plan.nodes, repeat.nodes);
    assert_eq!(plan.nodes.len(), 1);
    let node = &plan.nodes[0];
    assert_eq!(
        node.status,
        if label == "healthy" {
            voom_plan::NodeStatus::NoOp
        } else {
            voom_plan::NodeStatus::Planned
        }
    );
    let payload = RemuxOperationPayload::try_from_value(&node.operation_payload).unwrap();
    assert_eq!(payload.source_media_snapshot_id, Some(snapshot.0));
    assert_eq!(
        node.operation_payload["defaults"],
        json!([
            {"target":"audio", "strategy":"first"}, {"target":"subtitle", "strategy":"none"}
        ])
    );
    let facts = stream_facts(&input).unwrap();
    let retained = resolve_track_keep_ids(&facts, &payload.track_actions).unwrap();
    let expected: BTreeSet<_> = kept.iter().map(|index| format!("stream-{index}")).collect();
    assert_eq!(
        retained, expected,
        "case-specific supported policy selection for {label}"
    );
}
