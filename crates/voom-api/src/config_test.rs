use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use clap::Parser;

use super::{Cli, ServerLimits, TransportConfig};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn expected_failure(message: &'static str) -> Box<dyn std::error::Error> {
    std::io::Error::other(message).into()
}

#[test]
fn explicit_cleartext_accepts_ipv4_and_ipv6_loopback() -> TestResult {
    for bind in ["127.0.0.1:7443", "[::1]:7443"] {
        let config =
            Cli::try_parse_from(["voom-api", "--bind", bind, "--allow-cleartext-loopback"])
                .map_err(Box::<dyn std::error::Error>::from)?
                .validate()?;

        assert!(matches!(
            config.transport(),
            TransportConfig::CleartextLoopback
        ));
    }

    Ok(())
}

#[test]
fn explicit_cleartext_rejects_every_non_loopback_class() -> TestResult {
    for bind in [
        "0.0.0.0:7443",
        "192.168.1.20:7443",
        "169.254.1.1:7443",
        "203.0.113.10:7443",
        "[::]:7443",
    ] {
        let result =
            Cli::try_parse_from(["voom-api", "--bind", bind, "--allow-cleartext-loopback"])?
                .validate();
        let Err(error) = result else {
            return Err(expected_failure("expected non-loopback bind rejection"));
        };

        assert_eq!(error.code(), "CONFIG_INVALID");
        assert!(
            error
                .to_string()
                .contains("cleartext requires a loopback --bind")
        );
        assert!(!error.to_string().contains(bind));
    }

    Ok(())
}

#[test]
fn complete_tls_pair_accepts_non_loopback_bind() -> TestResult {
    let config = Cli::try_parse_from([
        "voom-api",
        "--bind",
        "0.0.0.0:7443",
        "--tls-cert",
        "server.pem",
        "--tls-key",
        "server.key",
    ])
    .map_err(Box::<dyn std::error::Error>::from)?
    .validate()?;

    let TransportConfig::Tls {
        cert_path,
        key_path,
    } = config.transport()
    else {
        return Err(expected_failure("expected TLS transport"));
    };
    assert_eq!(cert_path.to_string_lossy(), "server.pem");
    assert_eq!(key_path.to_string_lossy(), "server.key");

    Ok(())
}

#[test]
fn missing_partial_and_conflicting_transport_inputs_fail() -> TestResult {
    for args in [
        vec!["voom-api"],
        vec!["voom-api", "--tls-cert", "server.pem"],
        vec!["voom-api", "--tls-key", "server.key"],
        vec![
            "voom-api",
            "--tls-cert",
            "server.pem",
            "--tls-key",
            "server.key",
            "--allow-cleartext-loopback",
        ],
    ] {
        let result = Cli::try_parse_from(args)?.validate();
        let Err(error) = result else {
            return Err(expected_failure("expected invalid transport rejection"));
        };
        assert_eq!(error.code(), "CONFIG_INVALID");
        assert!(error.to_string().contains("select exactly one transport"));
    }

    Ok(())
}

#[test]
fn default_bind_is_safe_loopback() -> TestResult {
    let config = Cli::try_parse_from(["voom-api", "--allow-cleartext-loopback"])
        .map_err(Box::<dyn std::error::Error>::from)?
        .validate()?;

    assert_eq!(
        config.bind(),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7443)
    );

    Ok(())
}

#[test]
fn limits_reject_each_zero_value() -> TestResult {
    let one = Duration::from_secs(1);
    let cases = [
        (0, one, one, one, one, one),
        (1, Duration::ZERO, one, one, one, one),
        (1, one, Duration::ZERO, one, one, one),
        (1, one, one, Duration::ZERO, one, one),
        (1, one, one, one, Duration::ZERO, one),
        (1, one, one, one, one, Duration::ZERO),
    ];

    for (body, handshake, head, processing, connection, shutdown) in cases {
        let result =
            ServerLimits::new_for_test(body, handshake, head, processing, connection, shutdown);
        let Err(error) = result else {
            return Err(expected_failure("expected zero server limit rejection"));
        };
        assert_eq!(error.code(), "CONFIG_INVALID");
        assert!(error.to_string().contains("server limits must be non-zero"));
    }

    Ok(())
}
