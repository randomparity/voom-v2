use std::time::Duration;

use chrono::Utc;
use voom_worker_protocol::{OperationKind, PercentBps, ProgressFrame, ProtocolError};

use crate::catalog::operation_name;

pub(crate) struct TimedDispatch {
    pub(crate) writer: voom_worker_protocol::http::StreamingFrameWriter,
    pub(crate) lease_id: voom_core::LeaseId,
    pub(crate) provider: String,
    pub(crate) operation: OperationKind,
    pub(crate) scenario: String,
    pub(crate) result_payload: serde_json::Value,
    pub(crate) duration_ms: u64,
    pub(crate) progress_interval_ms: u64,
}

impl TimedDispatch {
    pub(crate) async fn emit(mut self) {
        let mut seq = 0_u64;
        let mut elapsed_ms = 0_u64;
        while elapsed_ms < self.duration_ms {
            let percent = percent_for(elapsed_ms, self.duration_ms);
            let frame = progress_frame(
                self.lease_id,
                seq,
                Utc::now(),
                percent,
                &self.provider,
                self.operation,
                &self.scenario,
            );
            if self.writer.write_frame(&frame).is_err() {
                return;
            }
            seq += 1;

            let remaining_ms = self.duration_ms - elapsed_ms;
            let sleep_ms = self.progress_interval_ms.min(remaining_ms);
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
            elapsed_ms += sleep_ms;
        }

        let result = ProgressFrame::Result {
            lease_id: self.lease_id,
            seq,
            emitted_at: Utc::now(),
            payload: self.result_payload,
        };
        if self.writer.write_frame(&result).is_ok() {
            let _ = self.writer.finish();
        }
    }
}

pub(crate) fn progress_frame(
    lease_id: voom_core::LeaseId,
    seq: u64,
    emitted_at: chrono::DateTime<Utc>,
    percent: PercentBps,
    provider: &str,
    operation: OperationKind,
    scenario: &str,
) -> ProgressFrame {
    ProgressFrame::Progress {
        lease_id,
        seq,
        emitted_at,
        percent: Some(percent),
        message: Some(format!(
            "{} handling {}",
            provider,
            operation_name(operation)
        )),
        payload: Some(serde_json::json!({
            "provider": provider,
            "operation": operation_name(operation),
            "scenario": scenario,
        })),
    }
}

pub(crate) fn body_from_frames(frames: &[ProgressFrame]) -> Result<Vec<u8>, ProtocolError> {
    let mut body = Vec::new();
    for frame in frames {
        let line = serde_json::to_vec(frame).map_err(|e| ProtocolError::InvalidPayload {
            detail: format!("frame encode: {e}"),
        })?;
        body.extend_from_slice(&line);
        body.push(b'\n');
    }
    Ok(body)
}

fn percent_for(elapsed_ms: u64, duration_ms: u64) -> PercentBps {
    if duration_ms == 0 {
        return PercentBps::FULL;
    }
    let bps = elapsed_ms.saturating_mul(10_000) / duration_ms;
    PercentBps::try_from(u16::try_from(bps).unwrap_or(10_000)).unwrap_or(PercentBps::FULL)
}
