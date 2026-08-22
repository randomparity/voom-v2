//! Scan-session client methods over the node token envelope transport (ADR
//! 0077). The pump drives four durable routes — start, batch, complete, fail —
//! with the same frozen-request retry discipline as every other control-plane
//! call. Wire types mirror `voom-api`'s route contracts exactly; both ends
//! reject unknown fields, so a drift between them fails loudly instead of
//! silently dropping evidence.

use serde::{Deserialize, Serialize};
use voom_core::{
    NodeIncarnationId, ScanObservationEvidence, ScanSessionId, ScanSessionStatus, VoomError,
};

use crate::client::{ControlPlaneClient, RetryRequest};

/// Wire body of `POST /v1/scan/node/{node}/session/{session}/start`.
#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScanStartRequest {
    pub incarnation_id: NodeIncarnationId,
}

/// One observation on the batch wire. Evidence rides verbatim after the API's
/// structural validation; the agent never classifies it further.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanObservationWire {
    pub provider_relative_locator: String,
    pub provider_object_identity: String,
    pub size_bytes: u64,
    pub modified_at: String,
    pub stability_started_at: String,
    pub stability_confirmed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<ScanObservationEvidence>,
}

/// Wire body of `POST .../batch/{sequence}`.
#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScanBatchRequest {
    pub incarnation_id: NodeIncarnationId,
    pub observations: Vec<ScanObservationWire>,
}

/// Wire body of `POST .../complete`.
#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScanCompleteRequest {
    pub incarnation_id: NodeIncarnationId,
    pub last_sequence: Option<u64>,
    pub observation_count: u64,
}

/// Wire body of `POST .../fail`.
#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScanFailRequest {
    pub incarnation_id: NodeIncarnationId,
    pub reason: String,
}

/// Success envelope of the start route.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanStartOutcome {
    pub scan_session_id: ScanSessionId,
    pub status: ScanSessionStatus,
    pub owner_incarnation_id: NodeIncarnationId,
    pub location_high_watermark_id: Option<u64>,
    pub progress_deadline_at: String,
}

/// Success envelope of the batch route.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanBatchOutcome {
    pub scan_session_id: ScanSessionId,
    pub sequence: u64,
    pub accepted_observation_count: u64,
    pub cumulative_observation_count: u64,
}

/// Success envelope of the fail route.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanTerminalOutcome {
    pub scan_session_id: ScanSessionId,
    pub status: ScanSessionStatus,
    pub terminal_at: String,
    pub terminal_reason: String,
}

/// Success envelope of the complete route: publication counters ride beside
/// the status transition even though the agent never reads them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanCompleteOutcome {
    pub scan_session_id: ScanSessionId,
    pub status: ScanSessionStatus,
    pub observation_count: u64,
    pub retired_location_count: u64,
}

impl ControlPlaneClient {
    /// Start one durable scan session owned by this incarnation.
    ///
    /// # Errors
    ///
    /// Returns the typed terminal error for conflicts (a session that is no
    /// longer `requested`) and transport failures otherwise.
    pub async fn start_scan_session(
        &self,
        node_id: u64,
        scan_session_id: ScanSessionId,
        request: &RetryRequest<ScanStartRequest>,
    ) -> Result<ScanStartOutcome, VoomError> {
        self.send(
            &format!(
                "/v1/scan/node/{node_id}/session/{}/start",
                scan_session_id.0
            ),
            request,
        )
        .await
    }

    /// Submit one ordered observation batch. The frozen request replays
    /// idempotently, so a retry after an unanswered success is safe.
    ///
    /// # Errors
    ///
    /// Returns the typed terminal error for sequence conflicts and transport
    /// failures otherwise.
    pub async fn submit_scan_batch(
        &self,
        node_id: u64,
        scan_session_id: ScanSessionId,
        sequence: u64,
        request: &RetryRequest<ScanBatchRequest>,
    ) -> Result<ScanBatchOutcome, VoomError> {
        self.send(
            &format!(
                "/v1/scan/node/{node_id}/session/{}/batch/{sequence}",
                scan_session_id.0
            ),
            request,
        )
        .await
    }

    /// Complete a finished session; publication happens control-plane side.
    ///
    /// # Errors
    ///
    /// Returns the typed terminal error for conflicts and transport failures.
    pub async fn complete_scan_session(
        &self,
        node_id: u64,
        scan_session_id: ScanSessionId,
        request: &RetryRequest<ScanCompleteRequest>,
    ) -> Result<ScanCompleteOutcome, VoomError> {
        self.send(
            &format!(
                "/v1/scan/node/{node_id}/session/{}/complete",
                scan_session_id.0
            ),
            request,
        )
        .await
    }

    /// Fail a session with a bounded operator-readable reason.
    ///
    /// # Errors
    ///
    /// Returns the typed terminal error for conflicts and transport failures.
    pub async fn fail_scan_session(
        &self,
        node_id: u64,
        scan_session_id: ScanSessionId,
        request: &RetryRequest<ScanFailRequest>,
    ) -> Result<ScanTerminalOutcome, VoomError> {
        self.send(
            &format!("/v1/scan/node/{node_id}/session/{}/fail", scan_session_id.0),
            request,
        )
        .await
    }
}
