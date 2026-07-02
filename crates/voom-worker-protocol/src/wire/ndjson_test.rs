use super::*;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use time::OffsetDateTime;
use tokio::io::{AsyncRead, ReadBuf};
use voom_core::LeaseId;

use crate::{PercentBps, ProgressFrame, ProtocolError};

/// An `AsyncRead` that serves `remaining` non-newline bytes in bounded chunks
/// and records the total number of bytes actually read from it. Used to prove
/// the reader stops consuming input once a frame exceeds `max_frame_bytes`,
/// rather than buffering the whole (potentially unbounded) line first.
struct CountingReader {
    remaining: usize,
    served: Arc<AtomicUsize>,
}

impl AsyncRead for CountingReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.remaining == 0 {
            return Poll::Ready(Ok(())); // EOF
        }
        let src = [b'x'; 4096];
        let n = self.remaining.min(buf.remaining()).min(src.len());
        buf.put_slice(&src[..n]);
        self.remaining -= n;
        self.served.fetch_add(n, Ordering::SeqCst);
        Poll::Ready(Ok(()))
    }
}

fn fixed_time() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_779_192_000).unwrap()
}

fn progress(lease: LeaseId, seq: u64) -> ProgressFrame {
    ProgressFrame::Progress {
        lease_id: lease,
        seq,
        emitted_at: fixed_time(),
        percent: Some(PercentBps::ZERO),
        message: None,
        payload: None,
    }
}

fn result_frame(lease: LeaseId, seq: u64) -> ProgressFrame {
    ProgressFrame::Result {
        lease_id: lease,
        seq,
        emitted_at: fixed_time(),
        payload: serde_json::json!({"ok": true}),
    }
}

fn line_for(frame: &ProgressFrame) -> Vec<u8> {
    let mut v = serde_json::to_vec(frame).unwrap();
    v.push(b'\n');
    v
}

#[tokio::test]
async fn first_frame_seq_zero_ok() {
    let lease = LeaseId(1);
    let bytes = line_for(&progress(lease, 0));
    let mut reader = NdjsonReader::new(bytes.as_slice(), lease);
    let out = reader.next_frame().await.unwrap();
    assert!(matches!(
        out,
        NdjsonOutcome::Frame(ProgressFrame::Progress { seq: 0, .. })
    ));
}

#[tokio::test]
async fn first_frame_nonzero_seq_rejects() {
    let lease = LeaseId(1);
    let bytes = line_for(&progress(lease, 3));
    let mut reader = NdjsonReader::new(bytes.as_slice(), lease);
    let err = reader.next_frame().await.unwrap_err();
    assert!(matches!(
        err,
        ProtocolError::OutOfOrderFrame {
            expected_seq: 0,
            got_seq: 3
        }
    ));
}

#[tokio::test]
async fn duplicate_seq_dropped() {
    let lease = LeaseId(1);
    let mut bytes = line_for(&progress(lease, 0));
    bytes.extend_from_slice(&line_for(&progress(lease, 0))); // duplicate
    bytes.extend_from_slice(&line_for(&result_frame(lease, 1)));
    let mut reader = NdjsonReader::new(bytes.as_slice(), lease);
    let out1 = reader.next_frame().await.unwrap();
    assert!(matches!(out1, NdjsonOutcome::Frame(_)));
    // Second call should skip the duplicate and surface the terminal Result.
    let out2 = reader.next_frame().await.unwrap();
    assert!(matches!(
        out2,
        NdjsonOutcome::Terminated(ProgressFrame::Result { seq: 1, .. })
    ));
}

#[tokio::test]
async fn many_consecutive_duplicates_dropped_without_recursion() {
    // Regression for M11: dropped duplicate-seq frames must be consumed
    // iteratively, not by re-entering `next_frame` once per dropped frame.
    // A long run of duplicates between two real frames must still surface the
    // next valid frame, with no per-duplicate call-chain growth.
    let lease = LeaseId(1);
    let mut bytes = line_for(&progress(lease, 0));
    for _ in 0..50_000 {
        bytes.extend_from_slice(&line_for(&progress(lease, 0))); // duplicate
    }
    bytes.extend_from_slice(&line_for(&result_frame(lease, 1)));
    let mut reader = NdjsonReader::new(bytes.as_slice(), lease);

    let out1 = reader.next_frame().await.unwrap();
    assert!(matches!(out1, NdjsonOutcome::Frame(_)));
    let out2 = reader.next_frame().await.unwrap();
    assert!(matches!(
        out2,
        NdjsonOutcome::Terminated(ProgressFrame::Result { seq: 1, .. })
    ));
}

#[tokio::test]
async fn gap_in_seq_rejects() {
    let lease = LeaseId(1);
    let mut bytes = line_for(&progress(lease, 0));
    bytes.extend_from_slice(&line_for(&progress(lease, 2))); // gap (expected 1)
    let mut reader = NdjsonReader::new(bytes.as_slice(), lease);
    reader.next_frame().await.unwrap(); // seq 0
    let err = reader.next_frame().await.unwrap_err();
    assert!(matches!(
        err,
        ProtocolError::OutOfOrderFrame {
            expected_seq: 1,
            got_seq: 2
        }
    ));
}

#[tokio::test]
async fn wrong_lease_id_rejects() {
    let bytes = line_for(&progress(LeaseId(99), 0));
    let mut reader = NdjsonReader::new(bytes.as_slice(), LeaseId(1));
    let err = reader.next_frame().await.unwrap_err();
    assert!(matches!(
        err,
        ProtocolError::WrongLeaseId {
            expected: LeaseId(1),
            got: LeaseId(99)
        }
    ));
}

#[tokio::test]
async fn frame_too_large_rejects() {
    let lease = LeaseId(1);
    let big_line = vec![b'x'; 200];
    let bytes: Vec<u8> = {
        let mut b = big_line;
        b.push(b'\n');
        b
    };
    let mut reader = NdjsonReader::new(bytes.as_slice(), lease).with_max_frame_bytes(64);
    let err = reader.next_frame().await.unwrap_err();
    assert!(matches!(
        err,
        ProtocolError::FrameTooLarge {
            bytes: 200,
            max: 64
        }
    ));
}

#[tokio::test]
async fn frame_too_large_stops_reading_before_buffering_whole_line() {
    // A worker that emits bytes without a newline must not force the whole line
    // into memory before FrameTooLarge fires: max_frame_bytes must bound the
    // allocation, not just the rejection. Feed a stream far larger than the
    // frame bound and assert the reader consumes only a bounded prefix.
    const N: usize = 4 * 1024 * 1024;
    let lease = LeaseId(1);
    let served = Arc::new(AtomicUsize::new(0));
    let reader = CountingReader {
        remaining: N,
        served: Arc::clone(&served),
    };
    let mut ndjson = NdjsonReader::new(reader, lease).with_max_frame_bytes(1024);

    let err = ndjson.next_frame().await.unwrap_err();
    assert!(
        matches!(err, ProtocolError::FrameTooLarge { .. }),
        "got {err:?}"
    );

    let total = served.load(Ordering::SeqCst);
    assert!(
        total < 64 * 1024,
        "reader consumed {total} bytes before rejecting; expected a bounded read well under N={N}"
    );
}

#[tokio::test]
async fn terminal_then_next_call_is_unexpected() {
    let lease = LeaseId(1);
    let bytes = line_for(&result_frame(lease, 0));
    let mut reader = NdjsonReader::new(bytes.as_slice(), lease);
    let out = reader.next_frame().await.unwrap();
    assert!(matches!(out, NdjsonOutcome::Terminated(_)));
    let err = reader.next_frame().await.unwrap_err();
    assert!(matches!(err, ProtocolError::UnexpectedFrameAfterTerminal));
}

#[tokio::test]
async fn eof_without_terminal_yields_stream_end() {
    let lease = LeaseId(1);
    let bytes = line_for(&progress(lease, 0));
    let mut reader = NdjsonReader::new(bytes.as_slice(), lease);
    reader.next_frame().await.unwrap(); // seq 0
    let out = reader.next_frame().await.unwrap();
    assert!(matches!(out, NdjsonOutcome::StreamEnd));
}

#[tokio::test]
async fn eof_mid_frame_rejects_as_malformed() {
    let lease = LeaseId(1);
    // No trailing newline; reader returns MalformedFrame (truncated stream).
    let bytes = b"{\"kind\":\"progress\",\"lease_id\":1".to_vec();
    let mut reader = NdjsonReader::new(bytes.as_slice(), lease);
    let err = reader.next_frame().await.unwrap_err();
    assert!(
        matches!(err, ProtocolError::MalformedFrame { ref detail } if detail.contains("truncated")),
        "got {err:?}"
    );
}

#[tokio::test]
async fn malformed_json_rejects() {
    let lease = LeaseId(1);
    let bytes = b"not json\n".to_vec();
    let mut reader = NdjsonReader::new(bytes.as_slice(), lease);
    let err = reader.next_frame().await.unwrap_err();
    assert!(matches!(err, ProtocolError::MalformedFrame { .. }));
}

#[tokio::test]
async fn writer_assigns_monotonic_seq() {
    let lease = LeaseId(1);
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut writer = NdjsonWriter::new(&mut buf, lease);
        // Caller-supplied seq is ignored; writer reassigns starting at 0.
        writer.emit(progress(lease, 99)).await.unwrap();
        writer.emit(result_frame(lease, 99)).await.unwrap();
    }
    let lines: Vec<&[u8]> = buf
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(lines.len(), 2);
    let f0: ProgressFrame = serde_json::from_slice(lines[0]).unwrap();
    let f1: ProgressFrame = serde_json::from_slice(lines[1]).unwrap();
    assert_eq!(f0.seq(), 0);
    assert_eq!(f1.seq(), 1);
}

#[tokio::test]
async fn writer_rejects_second_terminal() {
    let lease = LeaseId(1);
    let mut buf: Vec<u8> = Vec::new();
    let mut writer = NdjsonWriter::new(&mut buf, lease);
    writer.emit(result_frame(lease, 0)).await.unwrap();
    let err = writer.emit(result_frame(lease, 1)).await.unwrap_err();
    assert!(matches!(err, ProtocolError::MalformedFrame { .. }));
}

#[tokio::test]
async fn writer_rejects_wrong_lease_id() {
    let lease = LeaseId(1);
    let other = LeaseId(2);
    let mut buf: Vec<u8> = Vec::new();
    let mut writer = NdjsonWriter::new(&mut buf, lease);
    let err = writer.emit(progress(other, 0)).await.unwrap_err();
    assert!(matches!(
        err,
        ProtocolError::WrongLeaseId {
            expected: LeaseId(1),
            got: LeaseId(2)
        }
    ));
}
