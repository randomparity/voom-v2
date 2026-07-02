#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;
use tempfile::NamedTempFile;
use time::OffsetDateTime;
use voom_control_plane::ControlPlane;
use voom_store::repo::bundles::{BundleMemberRole, NewAssetBundle};
use voom_store::repo::identity::{
    FileLocationKind, MediaWorkKind, NewFileLocation, NewFileVersion, NewMediaVariant,
    NewMediaWork, ProducedBy,
};

mod bundle_envelope {
    use super::*;

    const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

    struct Fixture {
        _tmp: NamedTempFile,
        url: String,
    }

    async fn fixture() -> Fixture {
        let tmp = NamedTempFile::new().unwrap();
        let url = voom_store::test_support::sqlite_url_for(tmp.path());
        voom_store::init(&url).await.unwrap();
        let cp = ControlPlane::open(&url).await.unwrap();

        let work = cp
            .create_media_work(NewMediaWork {
                kind: MediaWorkKind::Movie,
                display_title: "Solaris".to_owned(),
                provisional: false,
                created_at: T0,
            })
            .await
            .unwrap();
        let variant = cp
            .create_media_variant(NewMediaVariant {
                media_work_id: work.id,
                label: "original".to_owned(),
                provisional: false,
                created_at: T0,
            })
            .await
            .unwrap();
        let bundle = cp
            .create_bundle(NewAssetBundle {
                media_variant_id: variant.id,
                display_name: "Solaris (1972)".to_owned(),
                created_at: T0,
            })
            .await
            .unwrap();

        // Primary video: an ingested file version with a local location.
        seed_ingest_member(
            &cp,
            bundle.id,
            BundleMemberRole::PrimaryVideo,
            "sha256:primary",
            1000,
            "/library/Solaris.mkv",
        )
        .await;
        // External subtitle: another ingested file.
        seed_ingest_member(
            &cp,
            bundle.id,
            BundleMemberRole::ExternalSubtitle,
            "sha256:subtitle",
            20,
            "/library/Solaris.srt",
        )
        .await;
        // Generated member: extracted audio whose live version was produced by
        // a transcode from the asset's original ingest version — exercising
        // provenance (produced_by + produced_from_version_id).
        seed_generated_member(&cp, bundle.id).await;

        Fixture { _tmp: tmp, url }
    }

    async fn seed_ingest_member(
        cp: &ControlPlane,
        bundle_id: voom_core::BundleId,
        role: BundleMemberRole,
        content_hash: &str,
        size_bytes: u64,
        location_value: &str,
    ) {
        let asset = cp.create_file_asset(T0).await.unwrap();
        let version = cp
            .create_file_version(NewFileVersion {
                file_asset_id: asset.id,
                content_hash: content_hash.to_owned(),
                size_bytes,
                produced_by: ProducedBy::Ingest,
                produced_from_version_id: None,
                created_at: T0,
            })
            .await
            .unwrap();
        cp.create_file_location(NewFileLocation {
            file_version_id: version.id,
            kind: FileLocationKind::LocalPath,
            value: location_value.to_owned(),
            proof: None,
            observed_at: T0,
        })
        .await
        .unwrap();
        cp.add_bundle_member(bundle_id, asset.id, role, T0)
            .await
            .unwrap();
    }

    async fn seed_generated_member(cp: &ControlPlane, bundle_id: voom_core::BundleId) {
        let asset = cp.create_file_asset(T0).await.unwrap();
        let ingest = cp
            .create_file_version(NewFileVersion {
                file_asset_id: asset.id,
                content_hash: "sha256:audio-source".to_owned(),
                size_bytes: 512,
                produced_by: ProducedBy::Ingest,
                produced_from_version_id: None,
                created_at: T0,
            })
            .await
            .unwrap();
        let transcoded = cp
            .create_file_version(NewFileVersion {
                file_asset_id: asset.id,
                content_hash: "sha256:audio".to_owned(),
                size_bytes: 300,
                produced_by: ProducedBy::Transcode,
                produced_from_version_id: Some(ingest.id),
                created_at: T0,
            })
            .await
            .unwrap();
        cp.create_file_location(NewFileLocation {
            file_version_id: transcoded.id,
            kind: FileLocationKind::LocalPath,
            value: "/library/Solaris.eac3".to_owned(),
            proof: None,
            observed_at: T0,
        })
        .await
        .unwrap();
        cp.add_bundle_member(bundle_id, asset.id, BundleMemberRole::ExternalAudio, T0)
            .await
            .unwrap();
    }

    fn bundle_command(url: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
        command.args(["--database-url", url, "bundle"]);
        command
    }

    fn envelope(stdout: Vec<u8>) -> Value {
        let stdout = String::from_utf8(stdout).unwrap();
        serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
    }

    fn redact_local(json: &mut Value) {
        json["local"]["db_url"] = Value::String("[db-url]".to_owned());
        json["local"]["config_path"] = Value::String("[config-path]".to_owned());
    }

    #[tokio::test]
    async fn bundle_list_outputs_member_counts() {
        let fixture = fixture().await;

        let output = bundle_command(&fixture.url).arg("list").output().unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "bundle");
        assert_eq!(json["status"], "ok");
        redact_local(&mut json);
        insta::assert_json_snapshot!("bundle_list_outputs_member_counts", json);
    }

    #[tokio::test]
    async fn bundle_show_outputs_members_lineage_and_provenance() {
        let fixture = fixture().await;

        let output = bundle_command(&fixture.url)
            .args(["show", "--bundle-id", "1"])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "bundle");
        assert_eq!(json["status"], "ok");
        redact_local(&mut json);
        insta::assert_json_snapshot!("bundle_show_outputs_members_lineage_and_provenance", json);
    }

    #[tokio::test]
    async fn bundle_show_unknown_id_is_not_found() {
        let fixture = fixture().await;

        let output = bundle_command(&fixture.url)
            .args(["show", "--bundle-id", "999"])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(2));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "bundle");
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"]["code"], "NOT_FOUND");
        redact_local(&mut json);
        insta::assert_json_snapshot!("bundle_show_unknown_id_is_not_found", json);
    }
}
