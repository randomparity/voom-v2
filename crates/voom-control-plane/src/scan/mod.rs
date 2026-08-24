//! Scan execution: durable scan sessions requested against configured roots,
//! executed by owner-node workers, and published from agreed evidence.
//!
//! `run` requests runs (fail-closed availability, ADR 0027); `sessions` owns
//! the durable session state machine and its remote routes' inputs; `publish`
//! turns evidence into identity inside the completion transaction.

pub(crate) mod publish;
pub mod run;
pub mod sessions;

pub use run::{RootBlockReason, RootScanBlocked, ScanRunOutcome, ScanRunRequested};
pub use sessions::{
    RemoteScanBatchInput, RemoteScanBatchOutcome, RemoteScanCompleteInput, RemoteScanFailInput,
    RemoteScanInspectInput, RemoteScanReconciliationInput, RemoteScanStartInput,
    RemoteScanStartOutcome, RemoteScanTerminalOutcome, ScanObservation, ScanReconciliationEvidence,
    ScanReconciliationPage, ScanReconciliationQuery, ScanSession, ScanSessionListQuery,
    ScanSessionPage,
};
