use super::*;

#[test]
fn parse_integer_env_reports_variable_and_value() {
    let err = parse_integer_env(WORKER_ID_ENV, "abc".to_owned()).unwrap_err();

    assert!(matches!(
        err,
        WorkerStartupError::InvalidIntegerEnv {
            name: WORKER_ID_ENV,
            ref value,
            ..
        } if value == "abc"
    ));
    assert!(err.to_string().contains(WORKER_ID_ENV));
}

#[test]
fn parse_bind_addr_accepts_loopback_ephemeral_port() {
    let addr = parse_bind_addr(WORKER_BIND_ENV, DEFAULT_WORKER_BIND.to_owned()).unwrap();

    assert_eq!(addr.ip().to_string(), "127.0.0.1");
    assert_eq!(addr.port(), 0);
}

#[test]
fn parse_bind_addr_reports_variable_and_value() {
    let err = parse_bind_addr(WORKER_BIND_ENV, "not-an-address".to_owned()).unwrap_err();

    assert!(matches!(
        err,
        WorkerStartupError::InvalidBindAddress {
            name: WORKER_BIND_ENV,
            ref value,
            ..
        } if value == "not-an-address"
    ));
    assert!(err.to_string().contains(WORKER_BIND_ENV));
}
