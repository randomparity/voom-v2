use std::future::Future;
use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use voom_api::config::{Cli, ServerConfig};
use voom_api::router_with_control_plane;
use voom_api::server::{RunningServer, ServerError};
use voom_control_plane::{ControlPlane, HealthPlane};
use voom_core::{Config, ErrorCode, VoomError};

#[derive(Clone, Copy, Debug)]
struct StartupDiagnostic {
    operation: &'static str,
    code: &'static str,
    message: &'static str,
}

impl StartupDiagnostic {
    const fn new(operation: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            operation,
            code,
            message,
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    init_tracing();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(diagnostic) => {
            tracing::error!(
                event = "startup_failed",
                operation = diagnostic.operation,
                code = diagnostic.code,
                message = diagnostic.message
            );
            ExitCode::FAILURE
        }
    }
}

fn init_tracing() {
    let level = std::env::var("VOOM_LOG_LEVEL").unwrap_or_else(|_| "info".to_owned());
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .json()
        .with_current_span(false)
        .init();
}

async fn run() -> Result<(), StartupDiagnostic> {
    let server_config = parse_server_config()?;
    let runtime_config =
        Config::resolve(server_config.database_url().map(str::to_owned), None, None).map_err(
            |_| {
                StartupDiagnostic::new(
                    "resolve_config",
                    ErrorCode::ConfigInvalid.as_str(),
                    concat!(
                        "failed to resolve runtime configuration; verify VOOM_DATABASE_URL ",
                        "and environment settings"
                    ),
                )
            },
        )?;
    let health_plane = HealthPlane::open(&runtime_config.database_url)
        .await
        .map_err(|error| database_diagnostic("open_health_database", &error))?;
    let control_plane = ControlPlane::open(&runtime_config.database_url)
        .await
        .map_err(|error| database_diagnostic("open_control_plane_database", &error))?;
    let termination = termination_signal()?;
    let router = router_with_control_plane(health_plane, control_plane);
    let transport_label = server_config.transport().label();
    let server = RunningServer::start(server_config, router)
        .await
        .map_err(|error| server_diagnostic(&error))?;
    tracing::info!(
        event = "listening",
        bind = %server.local_addr(),
        transport = transport_label
    );
    let termination_result = server
        .shutdown_on(termination)
        .await
        .map_err(|error| server_diagnostic(&error))?;
    termination_result?;
    tracing::info!(event = "shutdown_complete");
    Ok(())
}

fn parse_server_config() -> Result<ServerConfig, StartupDiagnostic> {
    Cli::try_parse()
        .map_err(|_| {
            StartupDiagnostic::new(
                "parse_arguments",
                ErrorCode::BadArgs.as_str(),
                "failed to parse API arguments; verify the supported flags and value formats",
            )
        })?
        .validate()
        .map_err(|_| {
            StartupDiagnostic::new(
                "validate_transport",
                ErrorCode::ConfigInvalid.as_str(),
                concat!(
                    "invalid API transport configuration: cleartext requires a loopback --bind; ",
                    "provide both --tls-cert and --tls-key for HTTPS"
                ),
            )
        })
}

fn database_diagnostic(operation: &'static str, error: &VoomError) -> StartupDiagnostic {
    StartupDiagnostic::new(
        operation,
        error.error_code().as_str(),
        concat!(
            "failed to open the existing API database; run `voom init`, then verify ",
            "VOOM_DATABASE_URL and file permissions"
        ),
    )
}

fn server_diagnostic(error: &ServerError) -> StartupDiagnostic {
    match error {
        ServerError::TlsConfig => StartupDiagnostic::new(
            "load_tls_identity",
            ErrorCode::ConfigInvalid.as_str(),
            concat!(
                "failed to load TLS identity; verify --tls-cert and --tls-key readability, ",
                "PEM format, chain order, and key match"
            ),
        ),
        ServerError::Bind(_) => StartupDiagnostic::new(
            "bind_listener",
            ErrorCode::Internal.as_str(),
            "failed to bind the API listener; verify --bind and local socket permissions",
        ),
        ServerError::Serve(_) | ServerError::Join(_) | ServerError::Stopped => {
            StartupDiagnostic::new(
                "serve_connections",
                ErrorCode::Internal.as_str(),
                concat!(
                    "the API server stopped unexpectedly; inspect host resources ",
                    "and restart the process"
                ),
            )
        }
    }
}

#[cfg(unix)]
fn termination_signal()
-> Result<impl Future<Output = Result<(), StartupDiagnostic>>, StartupDiagnostic> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut interrupt = signal(SignalKind::interrupt()).map_err(signal_setup_diagnostic)?;
    let mut terminate = signal(SignalKind::terminate()).map_err(signal_setup_diagnostic)?;
    Ok(termination_signal_with(
        async move { interrupt.recv().await },
        async move { terminate.recv().await },
    ))
}

#[cfg(unix)]
async fn termination_signal_with<I, T>(interrupt: I, terminate: T) -> Result<(), StartupDiagnostic>
where
    I: Future<Output = Option<()>>,
    T: Future<Output = Option<()>>,
{
    tokio::pin!(interrupt);
    tokio::pin!(terminate);
    let signal = tokio::select! {
        signal = &mut interrupt => signal,
        signal = &mut terminate => signal,
    };
    signal.ok_or_else(signal_closed_diagnostic)
}

#[cfg(not(unix))]
fn termination_signal()
-> Result<impl Future<Output = Result<(), StartupDiagnostic>>, StartupDiagnostic> {
    Ok(async {
        tokio::signal::ctrl_c()
            .await
            .map_err(signal_setup_diagnostic)
    })
}

fn signal_setup_diagnostic(_: std::io::Error) -> StartupDiagnostic {
    StartupDiagnostic::new(
        "install_termination_signal",
        ErrorCode::Internal.as_str(),
        "failed to install termination signal handlers; verify the process environment and restart",
    )
}

#[cfg(unix)]
const fn signal_closed_diagnostic() -> StartupDiagnostic {
    StartupDiagnostic::new(
        "await_termination_signal",
        ErrorCode::Internal.as_str(),
        "termination signal handling stopped unexpectedly; restart the process",
    )
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
