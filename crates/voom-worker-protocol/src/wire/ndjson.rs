//! NDJSON progress-stream codec. Phase 1 design §3.5.
//!
//! `NdjsonReader` and `NdjsonWriter` enforce the framing invariants
//! the rest of Sprint 2 relies on:
//!
//! - Every frame carries a monotonic `seq` starting at 0. Frames with
//!   `seq ≤ last_seq` are dropped; frames with `seq > last_seq + 1`
//!   abort the stream with `OutOfOrderFrame`.
//! - Every frame is bound to the reader's `expected_lease_id` (set at
//!   construction); a mismatching `lease_id` aborts the stream with
//!   `WrongLeaseId` BEFORE any seq/terminal check runs.
//! - Each lease's stream ends with exactly one terminal frame
//!   (`Result` or `Error`). Any frame after a terminal is
//!   `UnexpectedFrameAfterTerminal`.
//! - A single line longer than `max_frame_bytes` (default 64 KiB)
//!   aborts the stream with `FrameTooLarge`.
//! - EOF without a terminal frame yields `StreamEnd { partial_bytes }`.
//! - EOF mid-frame (partial JSON) is `MalformedFrame`.

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use voom_core::LeaseId;

use crate::{ProgressFrame, ProtocolError};

const DEFAULT_MAX_FRAME_BYTES: usize = 64 * 1024;

/// One outcome from `NdjsonReader::next_frame`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NdjsonOutcome {
    /// Non-terminal frame (`Progress`).
    Frame(ProgressFrame),
    /// Stream closed without a terminal frame (EOF). `partial_bytes`
    /// records how many bytes accumulated in the in-progress line buffer
    /// before EOF (zero for a clean close on a frame boundary).
    StreamEnd { partial_bytes: usize },
    /// Terminal frame (`Result` or `Error`) delivered. Subsequent
    /// `next_frame` calls return `UnexpectedFrameAfterTerminal`.
    Terminated(ProgressFrame),
}

pub struct NdjsonReader<R: AsyncRead + Unpin> {
    reader: BufReader<R>,
    expected_lease_id: LeaseId,
    last_seq: Option<u64>,
    terminal_seen: bool,
    max_frame_bytes: usize,
    line_buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> std::fmt::Debug for NdjsonReader<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NdjsonReader")
            .field("expected_lease_id", &self.expected_lease_id)
            .field("last_seq", &self.last_seq)
            .field("terminal_seen", &self.terminal_seen)
            .field("max_frame_bytes", &self.max_frame_bytes)
            .finish_non_exhaustive()
    }
}

impl<R: AsyncRead + Unpin> NdjsonReader<R> {
    #[must_use]
    pub fn new(reader: R, expected_lease_id: LeaseId) -> Self {
        Self {
            reader: BufReader::new(reader),
            expected_lease_id,
            last_seq: None,
            terminal_seen: false,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            line_buf: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_max_frame_bytes(mut self, max: usize) -> Self {
        self.max_frame_bytes = max;
        self
    }

    /// Read the next NDJSON frame. Returns one of:
    ///   - `Frame(progress)` — non-terminal frame; caller should call again.
    ///   - `Terminated(frame)` — terminal `Result` / `Error` delivered.
    ///   - `StreamEnd { partial_bytes }` — EOF before terminal.
    ///
    /// Calls after `Terminated` return
    /// `ProtocolError::UnexpectedFrameAfterTerminal`.
    ///
    /// On any contract violation, returns `Err(ProtocolError)` and the
    /// stream is considered aborted.
    pub async fn next_frame(&mut self) -> Result<NdjsonOutcome, ProtocolError> {
        if self.terminal_seen {
            return Err(ProtocolError::UnexpectedFrameAfterTerminal);
        }

        // Dropped duplicate-seq frames are consumed iteratively. Recursing per
        // dropped frame (even boxed) polls each call on the current stack when
        // reads resolve synchronously, so a long run of duplicates would
        // exhaust the stack and abort the task.
        loop {
            self.line_buf.clear();
            let n = self
                .reader
                .read_until(b'\n', &mut self.line_buf)
                .await
                .map_err(|e| ProtocolError::MalformedFrame {
                    detail: format!("read error: {e}"),
                })?;

            if n == 0 {
                // Clean EOF on a frame boundary.
                return Ok(NdjsonOutcome::StreamEnd { partial_bytes: 0 });
            }

            // Strip trailing newline if present.
            let had_newline = self.line_buf.last() == Some(&b'\n');
            let payload_len = if had_newline {
                self.line_buf.len() - 1
            } else {
                self.line_buf.len()
            };

            if payload_len > self.max_frame_bytes {
                return Err(ProtocolError::FrameTooLarge {
                    bytes: payload_len as u64,
                    max: self.max_frame_bytes as u64,
                });
            }

            if !had_newline {
                // EOF inside a frame without a terminating newline. The Phase 1
                // design treats this as MalformedFrame (truncated JSON), NOT
                // StreamEnd — a stream that ends mid-line is by definition
                // truncated, distinct from a clean close on a frame boundary.
                return Err(ProtocolError::MalformedFrame {
                    detail: format!(
                        "stream truncated mid-frame: {payload_len} bytes accumulated without newline"
                    ),
                });
            }

            let json_slice = &self.line_buf[..payload_len];
            let frame: ProgressFrame =
                serde_json::from_slice(json_slice).map_err(|e| ProtocolError::MalformedFrame {
                    detail: format!("json decode: {e}"),
                })?;

            // Lease boundary BEFORE seq / terminal logic.
            if frame.lease_id() != self.expected_lease_id {
                return Err(ProtocolError::WrongLeaseId {
                    expected: self.expected_lease_id,
                    got: frame.lease_id(),
                });
            }

            // Seq monotonicity.
            let got_seq = frame.seq();
            match self.last_seq {
                None => {
                    if got_seq != 0 {
                        return Err(ProtocolError::OutOfOrderFrame {
                            expected_seq: 0,
                            got_seq,
                        });
                    }
                }
                Some(last) => {
                    if got_seq <= last {
                        // Duplicate / lower seq → drop and read the next line.
                        continue;
                    }
                    if got_seq != last + 1 {
                        return Err(ProtocolError::OutOfOrderFrame {
                            expected_seq: last + 1,
                            got_seq,
                        });
                    }
                }
            }
            self.last_seq = Some(got_seq);

            return if frame.is_terminal() {
                self.terminal_seen = true;
                Ok(NdjsonOutcome::Terminated(frame))
            } else {
                Ok(NdjsonOutcome::Frame(frame))
            };
        }
    }
}

pub struct NdjsonWriter<W: AsyncWrite + Unpin> {
    writer: W,
    bound_lease_id: LeaseId,
    next_seq: u64,
    terminal_sent: bool,
}

impl<W: AsyncWrite + Unpin> std::fmt::Debug for NdjsonWriter<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NdjsonWriter")
            .field("bound_lease_id", &self.bound_lease_id)
            .field("next_seq", &self.next_seq)
            .field("terminal_sent", &self.terminal_sent)
            .finish_non_exhaustive()
    }
}

impl<W: AsyncWrite + Unpin> NdjsonWriter<W> {
    #[must_use]
    pub fn new(writer: W, bound_lease_id: LeaseId) -> Self {
        Self {
            writer,
            bound_lease_id,
            next_seq: 0,
            terminal_sent: false,
        }
    }

    /// The next seq value the writer will assign on `emit`.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Write one frame. The caller does NOT set `seq`; the writer
    /// assigns it monotonically. The frame's `lease_id` must match
    /// `bound_lease_id`; mismatch returns `Err(WrongLeaseId)` and the
    /// frame is NOT written. A second terminal frame returns
    /// `Err(MalformedFrame)` and is NOT written.
    pub async fn emit(&mut self, mut frame: ProgressFrame) -> Result<(), ProtocolError> {
        if self.terminal_sent {
            return Err(ProtocolError::MalformedFrame {
                detail: "second terminal frame".to_owned(),
            });
        }
        if frame.lease_id() != self.bound_lease_id {
            return Err(ProtocolError::WrongLeaseId {
                expected: self.bound_lease_id,
                got: frame.lease_id(),
            });
        }
        // Re-assign seq using the writer's counter.
        set_frame_seq(&mut frame, self.next_seq);
        self.next_seq += 1;

        let mut bytes = serde_json::to_vec(&frame).map_err(|e| ProtocolError::MalformedFrame {
            detail: format!("json encode: {e}"),
        })?;
        bytes.push(b'\n');
        self.writer
            .write_all(&bytes)
            .await
            .map_err(|e| ProtocolError::MalformedFrame {
                detail: format!("write error: {e}"),
            })?;

        if frame.is_terminal() {
            self.terminal_sent = true;
        }
        Ok(())
    }

    pub async fn flush_and_close(mut self) -> std::io::Result<()> {
        self.writer.flush().await?;
        self.writer.shutdown().await
    }
}

fn set_frame_seq(frame: &mut ProgressFrame, seq: u64) {
    match frame {
        ProgressFrame::Progress { seq: s, .. }
        | ProgressFrame::Result { seq: s, .. }
        | ProgressFrame::Error { seq: s, .. } => *s = seq,
    }
}

#[cfg(test)]
#[path = "ndjson_test.rs"]
mod tests;
