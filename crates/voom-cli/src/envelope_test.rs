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
        local: None,
        warnings: Vec::new(),
        error: None,
    };
    let json = serde_json::to_value(&env).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["msg"], "hi");
    assert!(json.get("local").is_none());
}

#[test]
fn local_block_serializes_when_present() {
    let env = Envelope::<()> {
        schema_version: SCHEMA_VERSION,
        command: "test",
        status: Status::Ok,
        data: None,
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
