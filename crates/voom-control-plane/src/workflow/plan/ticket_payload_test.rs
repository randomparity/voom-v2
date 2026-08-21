use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;
use voom_core::{
    ArtifactAccessDeclaration, ArtifactAccessEntry, ArtifactAccessRight, ArtifactAccessTarget,
    FileLocationAccess, FileLocationId, OperationKind, StorageRootAccess, StorageRootId,
};

#[test]
fn workflow_ticket_payload_rejects_operation_mismatch() {
    let payload = WorkflowTicketPayload::new_for_test(
        "workflow-1",
        "plan-1",
        "probe",
        "file-000",
        OperationKind::ProbeFile,
        serde_json::json!({"path": "/library/file-000.mkv"}),
    );
    let encoded = payload.to_ticket_payload().unwrap();
    let err =
        WorkflowTicketPayload::parse_ticket("synthetic.workflow.operation.scan_library", encoded)
            .unwrap_err();
    assert!(err.to_string().contains("operation mismatch"));
}

#[test]
fn workflow_ticket_payload_rejects_rendered_operation_mismatch_on_encode() {
    let payload = WorkflowTicketPayload::new_for_test(
        "workflow-1",
        "plan-1",
        "probe",
        "file-000",
        OperationKind::ProbeFile,
        serde_json::json!({
            "operation": "scan_library",
            "path": "/library/file-000.mkv"
        }),
    );

    let err = payload.to_ticket_payload().unwrap_err();
    assert!(err.to_string().contains("operation mismatch"));
}

#[test]
fn workflow_ticket_payload_accepts_transcode_audio_operation_name() {
    let payload = WorkflowTicketPayload::new_for_test(
        "workflow-1",
        "plan-1",
        "audio",
        "file-000",
        OperationKind::TranscodeAudio,
        serde_json::json!({
            "operation": "transcode_audio",
            "path": "/library/file-000.mkv"
        }),
    );

    let encoded = payload.to_ticket_payload().unwrap();
    let parsed = WorkflowTicketPayload::parse_ticket(
        "synthetic.workflow.operation.transcode_audio",
        encoded,
    )
    .unwrap();

    assert_eq!(parsed.operation, OperationKind::TranscodeAudio);
}

#[test]
fn parse_ticket_rejects_unknown_field() {
    let payload = WorkflowTicketPayload::new_for_test(
        "wf",
        "plan",
        "node",
        "branch",
        OperationKind::ProbeFile,
        serde_json::json!({}),
    );
    let base = payload.to_ticket_payload().unwrap();
    // (1) base parses Ok — proves the rejection below is the only behavior change.
    assert!(
        WorkflowTicketPayload::parse_ticket(
            "synthetic.workflow.operation.probe_file",
            base.clone()
        )
        .is_ok(),
        "base ticket payload must parse Ok",
    );
    // (2) base + unknown field is rejected.
    let mut value = base;
    value
        .as_object_mut()
        .unwrap()
        .insert("rogue".into(), serde_json::json!(1));
    let parsed =
        WorkflowTicketPayload::parse_ticket("synthetic.workflow.operation.probe_file", value);
    assert!(parsed.is_err(), "unknown field must fail the typed parse");
}

#[test]
fn a_malformed_source_location_id_is_rejected_rather_than_widening_the_declaration() {
    // Present-but-unreadable must not degrade to the whole-root shape: that shape is
    // legitimate for the operation, so it would pass every remaining rule.
    for value in [
        serde_json::json!("7"),
        serde_json::json!(7.5),
        serde_json::json!(-1),
        serde_json::json!(0),
        serde_json::Value::Null,
        serde_json::json!(true),
    ] {
        let mut payload = WorkflowTicketPayload::new_for_test(
            "wf",
            "plan",
            "probe",
            "file-000",
            OperationKind::ProbeFile,
            serde_json::json!({"path": "/library/file-000.mkv"}),
        );
        payload.rendered_payload["source_location_id"] = value.clone();

        let error = payload.to_ticket_payload().unwrap_err();

        assert!(
            error.to_string().contains("source_location_id"),
            "{value} produced {error}"
        );
    }
}

#[test]
fn a_byte_touching_payload_without_a_declaration_is_rejected_on_both_sides() {
    let mut payload = WorkflowTicketPayload::new_for_test(
        "wf",
        "plan",
        "probe",
        "file-000",
        OperationKind::ProbeFile,
        serde_json::json!({"path": "/library/file-000.mkv"}),
    );
    let encoded = payload.to_ticket_payload().unwrap();
    payload.declared_artifact_access = None;

    let encode_error = payload.to_ticket_payload().unwrap_err();
    assert!(
        encode_error
            .to_string()
            .contains("requires declared_artifact_access"),
        "encode message was {encode_error}"
    );

    let mut stripped = encoded;
    stripped
        .as_object_mut()
        .unwrap()
        .remove("declared_artifact_access");
    let decode_error =
        WorkflowTicketPayload::parse_ticket("synthetic.workflow.operation.probe_file", stripped)
            .unwrap_err();
    assert!(
        decode_error
            .to_string()
            .contains("requires declared_artifact_access"),
        "decode message was {decode_error}"
    );
}

#[test]
fn a_non_byte_touching_payload_must_not_declare_artifact_access() {
    let mut payload = WorkflowTicketPayload::new_for_test(
        "wf",
        "plan",
        "quality",
        "file-000",
        OperationKind::ScoreQuality,
        serde_json::json!({"path": "/library/file-000.mkv", "profile": "default", "codec": "h264"}),
    );
    assert!(payload.declared_artifact_access.is_none());

    // Borrow a valid declaration from a byte-touching operation and attach it here.
    let donor = WorkflowTicketPayload::new_for_test(
        "wf",
        "plan",
        "probe",
        "file-000",
        OperationKind::ProbeFile,
        serde_json::json!({"path": "/library/file-000.mkv"}),
    );
    payload.declared_artifact_access = donor.declared_artifact_access;

    let error = payload.to_ticket_payload().unwrap_err();

    assert!(
        error
            .to_string()
            .contains("is not byte-touching and must not declare artifact access"),
        "message was {error}"
    );
}

#[test]
fn a_canonical_declaration_that_is_not_the_operations_own_is_rejected_on_both_sides() {
    // The reason the gate is an equality check against `declaration_for` rather than
    // a shape check. Both forgeries below are canonical in their own right, so a
    // shape check would admit both: the first is the exact threat the gate's doc
    // comment names -- read, write, delete for a probe_file ticket on its source.
    // `new_for_test` renders root 3 / location 7, which is what these must forge
    // against for the mismatch to be about rights and target alone.
    let widened_rights = vec![ArtifactAccessEntry {
        target: ArtifactAccessTarget::FileLocation(FileLocationAccess {
            storage_root_id: StorageRootId(3),
            file_location_id: FileLocationId(7),
        }),
        rights: vec![
            ArtifactAccessRight::Read,
            ArtifactAccessRight::Write,
            ArtifactAccessRight::Delete,
        ],
    }];
    // Correct rights for probe_file, attached to the whole root instead of the
    // location -- the other half of what equality binds.
    let wrong_target = vec![ArtifactAccessEntry {
        target: ArtifactAccessTarget::StorageRoot(StorageRootAccess {
            storage_root_id: StorageRootId(3),
        }),
        rights: vec![ArtifactAccessRight::Read],
    }];

    for (case, entries) in [
        ("widened rights", widened_rights),
        ("wrong target", wrong_target),
    ] {
        // Constructing through `new` is the assertion that the forgery is canonical:
        // it enforces every canonical-form rule and would fail here otherwise.
        let forged = ArtifactAccessDeclaration::new(entries).unwrap();

        let mut payload = WorkflowTicketPayload::new_for_test(
            "wf",
            "plan",
            "probe",
            "file-000",
            OperationKind::ProbeFile,
            serde_json::json!({"path": "/library/file-000.mkv"}),
        );
        let valid_wire = payload.to_ticket_payload().unwrap();
        payload.declared_artifact_access = Some(forged.clone());

        let encode_error = payload.to_ticket_payload().unwrap_err();
        assert!(
            encode_error
                .to_string()
                .contains("does not match the canonical declaration"),
            "{case}: encode message was {encode_error}"
        );

        let mut forged_wire = valid_wire;
        forged_wire.as_object_mut().unwrap().insert(
            "declared_artifact_access".to_owned(),
            serde_json::to_value(&forged).unwrap(),
        );
        let decode_error = WorkflowTicketPayload::parse_ticket(
            "synthetic.workflow.operation.probe_file",
            forged_wire,
        )
        .unwrap_err();
        assert!(
            decode_error
                .to_string()
                .contains("does not match the canonical declaration"),
            "{case}: decode message was {decode_error}"
        );
    }
}
