//! Worker identity (`worker_id` + `worker_epoch` + bearer secret) and the
//! `validate_credentials` middleware helper. Phase 1 design §3.4.
//!
//! `WorkerCredentials` is the live record built at spawn time. The
//! secret is wrapped in `secrecy::SecretString` so it zeroes on drop
//! and the custom Debug impl never prints it. `PresentedCredentials`
//! is the parsed-from-headers form on every callback / dispatch.
//! `validate_credentials` performs the three-field check (id, epoch,
//! bearer) and uses constant-time compare for the secret to prevent
//! timing oracles.

use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use voom_core::WorkerId;

use crate::ProtocolError;

/// Live worker identity, owned by the supervisor for one spawn.
pub struct WorkerCredentials {
    pub worker_id: WorkerId,
    pub worker_epoch: u64,
    pub secret: SecretString,
}

impl fmt::Debug for WorkerCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkerCredentials")
            .field("worker_id", &self.worker_id)
            .field("worker_epoch", &self.worker_epoch)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl Clone for WorkerCredentials {
    fn clone(&self) -> Self {
        Self {
            worker_id: self.worker_id,
            worker_epoch: self.worker_epoch,
            secret: self.secret.expose_secret().to_string().into(),
        }
    }
}

/// Parsed headers from an inbound request — the candidate identity
/// the receiver compares against its `WorkerCredentials`.
pub struct PresentedCredentials {
    pub worker_id: WorkerId,
    pub worker_epoch: u64,
    pub secret: SecretString,
}

impl fmt::Debug for PresentedCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PresentedCredentials")
            .field("worker_id", &self.worker_id)
            .field("worker_epoch", &self.worker_epoch)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Compare `presented` against `live`. Returns `Ok(())` only if all
/// three fields match. Uses constant-time compare on the secret.
///
/// Order of checks (matters for diagnostics — the most specific
/// failure wins):
///   1. `worker_id` mismatch → `UnknownWorkerId`
///   2. `worker_epoch` mismatch → `StaleWorkerEpoch`
///   3. secret mismatch → `UnauthorizedBearer`
pub fn validate_credentials(
    presented: &PresentedCredentials,
    live: &WorkerCredentials,
) -> Result<(), ProtocolError> {
    if presented.worker_id != live.worker_id {
        return Err(ProtocolError::UnknownWorkerId {
            presented: presented.worker_id,
        });
    }
    if presented.worker_epoch != live.worker_epoch {
        return Err(ProtocolError::StaleWorkerEpoch {
            presented: presented.worker_epoch,
            current: live.worker_epoch,
        });
    }
    let a = presented.secret.expose_secret().as_bytes();
    let b = live.secret.expose_secret().as_bytes();
    if !constant_time_eq::constant_time_eq(a, b) {
        return Err(ProtocolError::UnauthorizedBearer);
    }
    Ok(())
}

#[cfg(test)]
#[path = "credentials_test.rs"]
mod tests;
