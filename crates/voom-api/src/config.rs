use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use voom_core::VoomError;

const DEFAULT_BIND: &str = "127.0.0.1:7443";
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;
const TLS_HANDSHAKE_SECONDS: u64 = 30;
const REQUEST_HEAD_SECONDS: u64 = 30;
const REQUEST_PROCESSING_SECONDS: u64 = 30;
const CONNECTION_SECONDS: u64 = 90;
const SHUTDOWN_GRACE_SECONDS: u64 = 30;

#[derive(Parser, Debug)]
#[command(name = "voom-api", version, about = "VOOM control-plane API server")]
pub struct Cli {
    /// Override the database URL (default: XDG data directory).
    #[arg(long, env = "VOOM_DATABASE_URL")]
    database_url: Option<String>,

    /// TCP address for the API listener.
    #[arg(long, default_value = DEFAULT_BIND)]
    bind: SocketAddr,

    /// PEM certificate chain presented by the HTTPS server.
    #[arg(long)]
    tls_cert: Option<PathBuf>,

    /// PEM private key matching --tls-cert.
    #[arg(long)]
    tls_key: Option<PathBuf>,

    /// Explicitly allow HTTP when --bind is IPv4 or IPv6 loopback.
    #[arg(long)]
    allow_cleartext_loopback: bool,
}

impl Cli {
    /// Validate fail-closed transport configuration before any listener is created.
    pub fn validate(self) -> Result<ServerConfig, VoomError> {
        let transport = match (self.tls_cert, self.tls_key, self.allow_cleartext_loopback) {
            (Some(cert_path), Some(key_path), false) => TransportConfig::Tls {
                cert_path,
                key_path,
            },
            (None, None, true) if self.bind.ip().is_loopback() => {
                TransportConfig::CleartextLoopback
            }
            (None, None, true) => {
                return Err(VoomError::Config(
                    "cleartext requires a loopback --bind; configure --tls-cert and --tls-key \
                     for remote traffic"
                        .to_owned(),
                ));
            }
            _ => {
                return Err(VoomError::Config(
                    "select exactly one transport: provide both --tls-cert and --tls-key, or \
                     use --allow-cleartext-loopback with a loopback --bind"
                        .to_owned(),
                ));
            }
        };

        Ok(ServerConfig {
            database_url: self.database_url,
            bind: self.bind,
            transport,
            limits: ServerLimits::default(),
        })
    }
}

#[derive(Clone, Debug)]
pub enum TransportConfig {
    Tls {
        cert_path: PathBuf,
        key_path: PathBuf,
    },
    CleartextLoopback,
}

impl TransportConfig {
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Tls { .. } => "https",
            Self::CleartextLoopback => "http-loopback",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ServerLimits {
    pub(crate) max_request_body_bytes: usize,
    pub(crate) tls_handshake: Duration,
    pub(crate) request_head: Duration,
    pub(crate) request_processing: Duration,
    pub(crate) connection: Duration,
    pub(crate) shutdown_grace: Duration,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            max_request_body_bytes: MAX_REQUEST_BODY_BYTES,
            tls_handshake: Duration::from_secs(TLS_HANDSHAKE_SECONDS),
            request_head: Duration::from_secs(REQUEST_HEAD_SECONDS),
            request_processing: Duration::from_secs(REQUEST_PROCESSING_SECONDS),
            connection: Duration::from_secs(CONNECTION_SECONDS),
            shutdown_grace: Duration::from_secs(SHUTDOWN_GRACE_SECONDS),
        }
    }
}

impl ServerLimits {
    #[cfg(any(test, feature = "test"))]
    pub fn new_for_test(
        max_request_body_bytes: usize,
        tls_handshake: Duration,
        request_head: Duration,
        request_processing: Duration,
        connection: Duration,
        shutdown_grace: Duration,
    ) -> Result<Self, VoomError> {
        let limits = Self {
            max_request_body_bytes,
            tls_handshake,
            request_head,
            request_processing,
            connection,
            shutdown_grace,
        };
        if limits.max_request_body_bytes == 0
            || limits.tls_handshake.is_zero()
            || limits.request_head.is_zero()
            || limits.request_processing.is_zero()
            || limits.connection.is_zero()
            || limits.shutdown_grace.is_zero()
        {
            return Err(VoomError::Config(
                "server limits must be non-zero".to_owned(),
            ));
        }
        Ok(limits)
    }
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    database_url: Option<String>,
    bind: SocketAddr,
    transport: TransportConfig,
    limits: ServerLimits,
}

impl ServerConfig {
    #[must_use]
    pub fn database_url(&self) -> Option<&str> {
        self.database_url.as_deref()
    }

    #[must_use]
    pub const fn bind(&self) -> SocketAddr {
        self.bind
    }

    #[must_use]
    pub const fn transport(&self) -> &TransportConfig {
        &self.transport
    }

    #[must_use]
    pub const fn limits(&self) -> ServerLimits {
        self.limits
    }

    #[cfg(any(test, feature = "test"))]
    #[must_use]
    pub const fn with_limits_for_test(mut self, limits: ServerLimits) -> Self {
        self.limits = limits;
        self
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
