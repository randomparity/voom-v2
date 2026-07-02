use super::*;
use serde::Serialize;

#[derive(Serialize)]
struct Hello {
    msg: &'static str,
}

#[test]
fn ok_envelope_includes_status_ok() {
    let env = Envelope {
        schema_version: SCHEMA_VERSION,
        command: "test",
        status: Status::Ok,
        data: Some(Hello { msg: "hi" }),
        next_cursor: None,
        local: None,
        warnings: Vec::new(),
        error: None,
    };
    let json = serde_json::to_value(&env).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["msg"], "hi");
    assert!(json.get("local").is_none());
    assert!(json.get("next_cursor").is_none());
}

#[test]
fn next_cursor_serializes_when_present() {
    let env = Envelope {
        schema_version: SCHEMA_VERSION,
        command: "event",
        status: Status::Ok,
        data: Some(Hello { msg: "hi" }),
        next_cursor: Some(42),
        local: None,
        warnings: Vec::new(),
        error: None,
    };
    let json = serde_json::to_value(&env).unwrap();
    assert_eq!(json["next_cursor"], 42);
}

#[test]
fn local_block_serializes_when_present() {
    let env = Envelope::<()> {
        schema_version: SCHEMA_VERSION,
        command: "test",
        status: Status::Ok,
        data: None,
        next_cursor: None,
        local: Some(Local {
            db_url: "sqlite::memory:".into(),
            config_path: "/etc/voom".into(),
        }),
        warnings: Vec::new(),
        error: None,
    };
    let json = serde_json::to_value(&env).unwrap();
    assert_eq!(json["local"]["db_url"], "sqlite::memory:");
}

#[test]
fn error_envelope_omits_data() {
    let env: Envelope<()> = Envelope {
        schema_version: SCHEMA_VERSION,
        command: "test",
        status: Status::Error,
        data: None,
        next_cursor: None,
        local: None,
        warnings: Vec::new(),
        error: Some(ErrorBody {
            code: "DB_UNREACHABLE",
            message: "boom".into(),
            hint: None,
        }),
    };
    let json = serde_json::to_value(&env).unwrap();
    assert_eq!(json["status"], "error");
    assert!(json["data"].is_null());
    assert_eq!(json["error"]["code"], "DB_UNREACHABLE");
}

#[test]
fn error_envelope_can_include_partial_data() {
    let env = Envelope {
        schema_version: SCHEMA_VERSION,
        command: "compliance",
        status: Status::Error,
        data: Some(serde_json::json!({
            "report": {"report_id": "report_test"},
            "issues": {"created_count": 1},
            "execution_diagnostic": {"code": "unsupported_execution_operation"}
        })),
        next_cursor: None,
        local: None,
        warnings: Vec::new(),
        error: Some(ErrorBody {
            code: "POLICY_EXECUTION_ERROR",
            message: "unsupported".into(),
            hint: None,
        }),
    };
    let json = serde_json::to_value(&env).unwrap();

    assert_eq!(json["status"], "error");
    assert_eq!(json["data"]["report"]["report_id"], "report_test");
    assert_eq!(json["data"]["issues"]["created_count"], 1);
    assert_eq!(
        json["data"]["execution_diagnostic"]["code"],
        "unsupported_execution_operation"
    );
}
