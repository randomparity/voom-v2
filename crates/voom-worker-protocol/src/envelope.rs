//! Wire envelope types — `OperationRequest`, `OperationResponse`,
//! `ProgressFrame`, `ProtocolError`, plus `PercentBps` for the
//! `Progress::percent` field.
//!
//! See `docs/superpowers/specs/2026-05-19-voom-sprint-2-phase-1-design.md`
//! §3.2 for the full contract.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use voom_core::{LeaseId, WorkerId};

use crate::operation_kind::OperationKind;

/// 0..=10000 basis points so `Eq` is derivable and on-wire JSON is
/// integer (no NaN, no float-equality foot-guns). 0 → 0%, 10000 → 100%.
///
/// Field is private; only `TryFrom<u16>` and `Deserialize` can
/// construct one, so the `0..=10000` invariant is enforced at every
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct PercentBps {
    bps: u16,
}

impl PercentBps {
    pub const ZERO: Self = Self { bps: 0 };
    pub const FULL: Self = Self { bps: 10_000 };

    #[must_use]
    pub fn bps(self) -> u16 {
        self.bps
    }
}

impl TryFrom<u16> for PercentBps {
    type Error = ProtocolError;
    fn try_from(bps: u16) -> Result<Self, Self::Error> {
        if bps > 10_000 {
            Err(ProtocolError::InvalidPayload {
                detail: format!("percent_bps={bps} > 10000"),
            })
        } else {
            Ok(Self { bps })
        }
    }
}

impl From<PercentBps> for u16 {
    fn from(p: PercentBps) -> u16 {
        p.bps
    }
}

/// Top-level operation request from supervisor → worker.
///
/// The supervisor POSTs an `OperationRequest` to the worker's
/// `/v1/operations` endpoint. The HTTP response body is an NDJSON
/// stream of `ProgressFrame`s terminated by exactly one `Result` or
/// `Error` frame.
///
/// `lease_id` is the sole work-identity authority on the wire.
/// Idempotency is carried ONLY in the `X-Voom-Idempotency-Key`
/// request header (see `http.rs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationRequest {
    pub operation: OperationKind,
    pub lease_id: LeaseId,
    pub payload: serde_json::Value,
    pub heartbeat_deadline_ms: u32,
    pub progress_idle_deadline_ms: u32,
}

/// Worker → supervisor immediate ack on `/v1/operations`. The
/// supervisor verifies it before consuming the NDJSON body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationResponse {
    pub lease_id: LeaseId,
    pub accepted_at: DateTime<Utc>,
}

/// One frame on the NDJSON progress stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProgressFrame {
    Progress {
        lease_id: LeaseId,
        seq: u64,
        emitted_at: DateTime<Utc>,
        percent: Option<PercentBps>,
        message: Option<String>,
        payload: Option<serde_json::Value>,
    },
    Result {
        lease_id: LeaseId,
        seq: u64,
        emitted_at: DateTime<Utc>,
        payload: serde_json::Value,
    },
    Error {
        lease_id: LeaseId,
        seq: u64,
        emitted_at: DateTime<Utc>,
        class: voom_core::FailureClass,
        code: voom_core::ErrorCode,
        message: String,
        payload: Option<serde_json::Value>,
    },
}

impl ProgressFrame {
    /// The lease this frame is bound to, for boundary enforcement
    /// in `NdjsonReader` / `NdjsonWriter`.
    #[must_use]
    pub fn lease_id(&self) -> LeaseId {
        match self {
            Self::Progress { lease_id, .. }
            | Self::Result { lease_id, .. }
            | Self::Error { lease_id, .. } => *lease_id,
        }
    }

    #[must_use]
    pub fn seq(&self) -> u64 {
        match self {
            Self::Progress { seq, .. } | Self::Result { seq, .. } | Self::Error { seq, .. } => *seq,
        }
    }

    /// `true` iff this is the terminal frame (`Result` or `Error`).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Result { .. } | Self::Error { .. })
    }
}

/// Errors processing the wire contract itself (distinct from
/// `FailureClass`, which describes work outcomes).
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProtocolError {
    #[error(
        "unsupported protocol version: offered={offered}, supported [{supported_min}, {supported_max}]"
    )]
    UnsupportedProtocolVersion {
        offered: u32,
        supported_min: u32,
        supported_max: u32,
    },
    #[error("unknown operation: {name}")]
    UnknownOperation { name: String },
    #[error("invalid payload: {detail}")]
    InvalidPayload { detail: String },
    #[error("unauthorized bearer")]
    UnauthorizedBearer,
    #[error("unknown worker id: {presented:?}")]
    UnknownWorkerId { presented: WorkerId },
    #[error("stale worker epoch: presented={presented}, current={current}")]
    StaleWorkerEpoch { presented: u64, current: u64 },
    #[error("worker retired: worker={worker_id:?}, epoch={epoch}")]
    WorkerRetired { worker_id: WorkerId, epoch: u64 },
    #[error("duplicate idempotency key: {key}")]
    DuplicateIdempotencyKey {
        key: String,
        original_status: String,
    },
    #[error("frame too large: {bytes} bytes (max {max})")]
    FrameTooLarge { bytes: u64, max: u64 },
    #[error("malformed frame: {detail}")]
    MalformedFrame { detail: String },
    #[error("out of order frame: expected seq {expected_seq}, got {got_seq}")]
    OutOfOrderFrame { expected_seq: u64, got_seq: u64 },
    #[error("wrong lease id: expected {expected:?}, got {got:?}")]
    WrongLeaseId { expected: LeaseId, got: LeaseId },
    #[error("unexpected frame after terminal")]
    UnexpectedFrameAfterTerminal,
    #[error("body carries idempotency_key (header is canonical)")]
    HeaderBodyKeyMismatch,
    #[error("internal server error")]
    InternalServerError,
}

#[cfg(test)]
#[path = "envelope_test.rs"]
mod tests;
