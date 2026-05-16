use super::*;

#[test]
fn log_format_parses_text_and_json() {
    assert_eq!(LogFormat::parse("text").unwrap(), LogFormat::Text);
    assert_eq!(LogFormat::parse("json").unwrap(), LogFormat::Json);
}

#[test]
fn log_format_rejects_unknown() {
    let err = LogFormat::parse("xml").unwrap_err();
    assert_eq!(err.code(), "CONFIG_INVALID");
}

#[test]
fn override_takes_priority_over_env() {
    let env = MapEnv::new().with("VOOM_DATABASE_URL", "sqlite::env");
    let cfg = Config::resolve_from(&env, Some("sqlite::override".into()), None, None).unwrap();
    assert_eq!(cfg.database_url, "sqlite::override");
}

#[test]
fn env_used_when_no_override() {
    let env = MapEnv::new().with("VOOM_DATABASE_URL", "sqlite::env-value");
    let cfg = Config::resolve_from(&env, None, None, None).unwrap();
    assert_eq!(cfg.database_url, "sqlite::env-value");
}

#[test]
fn defaults_yield_sqlite_url_when_env_empty() {
    let env = MapEnv::new();
    let cfg = Config::resolve_from(&env, None, None, None).unwrap();
    assert!(cfg.database_url.starts_with("sqlite://"));
}

#[test]
fn log_format_env_parsed_into_enum() {
    let env = MapEnv::new().with("VOOM_LOG_FORMAT", "text");
    let cfg = Config::resolve_from(&env, None, None, None).unwrap();
    assert_eq!(cfg.log_format, LogFormat::Text);
}
