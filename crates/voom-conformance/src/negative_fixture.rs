use time::OffsetDateTime;
use tokio::io::{AsyncRead, AsyncWriteExt};
use voom_core::LeaseId;
use voom_worker_protocol::{NdjsonReader, ProgressFrame, ProtocolError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureMode {
    WrongLeaseId,
    FrameAfterTerminal,
    TruncatedBody,
}

pub async fn classify_fixture(mode: FixtureMode) -> Result<(), ProtocolError> {
    let expected = LeaseId(1);
    let bytes = fixture_bytes(mode, expected)?;
    if mode == FixtureMode::FrameAfterTerminal && !has_frame_after_terminal(&bytes, expected)? {
        return Err(ProtocolError::MalformedFrame {
            detail: "fixture missing frame after terminal".to_owned(),
        });
    }
    let (mut writer, reader) = tokio::io::duplex(bytes.len().saturating_add(1));
    writer
        .write_all(&bytes)
        .await
        .map_err(|e| ProtocolError::MalformedFrame {
            detail: format!("fixture write: {e}"),
        })?;
    drop(writer);
    classify_reader(reader, expected).await
}

pub async fn classify_reader<R>(reader: R, expected: LeaseId) -> Result<(), ProtocolError>
where
    R: AsyncRead + Unpin,
{
    let mut reader = NdjsonReader::new(reader, expected);
    loop {
        match reader.next_frame().await? {
            voom_worker_protocol::NdjsonOutcome::Frame(_) => {}
            voom_worker_protocol::NdjsonOutcome::Terminated(_) => {
                reader.next_frame().await?;
                return Ok(());
            }
            voom_worker_protocol::NdjsonOutcome::StreamEnd { .. } => {
                return Err(ProtocolError::MalformedFrame {
                    detail: "stream ended before terminal".to_owned(),
                });
            }
        }
    }
}

pub fn has_frame_after_terminal(bytes: &[u8], expected: LeaseId) -> Result<bool, ProtocolError> {
    let mut terminal_seen = false;
    for raw_line in bytes.split(|b| *b == b'\n') {
        if raw_line.is_empty() {
            continue;
        }
        let frame: ProgressFrame =
            serde_json::from_slice(raw_line).map_err(|e| ProtocolError::MalformedFrame {
                detail: format!("fixture scan decode: {e}"),
            })?;
        if frame.lease_id() != expected {
            return Err(ProtocolError::WrongLeaseId {
                expected,
                got: frame.lease_id(),
            });
        }
        if terminal_seen {
            return Ok(true);
        }
        terminal_seen = frame.is_terminal();
    }
    Ok(false)
}

pub fn fixture_bytes(mode: FixtureMode, expected: LeaseId) -> Result<Vec<u8>, ProtocolError> {
    let now = OffsetDateTime::now_utc();
    let mut bytes = Vec::new();
    let progress = ProgressFrame::Progress {
        lease_id: match mode {
            FixtureMode::WrongLeaseId => LeaseId(expected.0 + 1),
            _ => expected,
        },
        seq: 0,
        emitted_at: now,
        percent: None,
        message: Some("fixture".to_owned()),
        payload: None,
    };
    push_frame(&mut bytes, &progress)?;
    if mode == FixtureMode::WrongLeaseId {
        return Ok(bytes);
    }

    let result = ProgressFrame::Result {
        lease_id: expected,
        seq: 1,
        emitted_at: now,
        payload: serde_json::json!({"ok": true}),
    };
    push_frame(&mut bytes, &result)?;

    match mode {
        FixtureMode::FrameAfterTerminal => {
            let extra = ProgressFrame::Progress {
                lease_id: expected,
                seq: 2,
                emitted_at: now,
                percent: None,
                message: Some("after terminal".to_owned()),
                payload: None,
            };
            push_frame(&mut bytes, &extra)?;
        }
        FixtureMode::TruncatedBody => {
            bytes.pop();
        }
        FixtureMode::WrongLeaseId => {}
    }
    Ok(bytes)
}

fn push_frame(out: &mut Vec<u8>, frame: &ProgressFrame) -> Result<(), ProtocolError> {
    let mut bytes = serde_json::to_vec(frame).map_err(|e| ProtocolError::MalformedFrame {
        detail: format!("fixture encode: {e}"),
    })?;
    bytes.push(b'\n');
    out.extend(bytes);
    Ok(())
}

#[cfg(test)]
#[path = "negative_fixture_test.rs"]
mod tests;
