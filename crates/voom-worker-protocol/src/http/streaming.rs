use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body::{Body, Frame};
use http_body_util::{BodyExt, Full};
use hyper::Response;
use hyper::StatusCode;
use hyper::header::CONTENT_TYPE;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::{OperationResponse, ProgressFrame, ProtocolError};

use super::ResponseBody;
use super::idempotency::{CachedResponse, IdempotencyCache};

/// What the worker's handler returns from one `/v1/operations` call.
pub struct OperationDispatch {
    pub response: OperationResponse,
    pub body: OperationBody,
}

impl OperationDispatch {
    /// Build a dispatch with already-buffered NDJSON frame bytes.
    #[must_use]
    pub fn buffered(response: OperationResponse, body: Vec<u8>) -> Self {
        Self {
            response,
            body: OperationBody::Buffered(body),
        }
    }

    /// Build a dispatch whose NDJSON frame bytes are written live.
    #[must_use]
    pub fn streaming(response: OperationResponse) -> (StreamingFrameWriter, Self) {
        let (writer, body) = StreamingBody::new();
        (
            writer,
            Self {
                response,
                body: OperationBody::Streaming(body),
            },
        )
    }
}

/// NDJSON frame body returned by an operation handler.
pub enum OperationBody {
    Buffered(Vec<u8>),
    Streaming(StreamingBody),
}

impl std::fmt::Debug for OperationDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperationDispatch")
            .field("response", &self.response)
            .field("body", &self.body)
            .finish()
    }
}

impl std::fmt::Debug for OperationBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Buffered(body) => f
                .debug_struct("OperationBody::Buffered")
                .field("body_len", &body.len())
                .finish(),
            Self::Streaming(_) => f
                .debug_struct("OperationBody::Streaming")
                .finish_non_exhaustive(),
        }
    }
}

/// Writer half for a live operation response stream.
pub struct StreamingFrameWriter {
    sender: UnboundedSender<StreamingMessage>,
    shared: Arc<StreamingShared>,
}

impl StreamingFrameWriter {
    pub fn write_frame(&mut self, frame: &ProgressFrame) -> Result<(), ProtocolError> {
        // Reject any frame once a terminal has been sent. Without this guard a
        // second terminal frame is appended to the cached body, concatenating
        // two terminal frames and corrupting the idempotency-cache entry on
        // replay. Mirrors NdjsonWriter::emit.
        if self.shared.terminal_sent.load(Ordering::SeqCst) {
            return Err(ProtocolError::MalformedFrame {
                detail: "second terminal frame".to_owned(),
            });
        }
        let terminal = frame.is_terminal();
        let mut bytes = serde_json::to_vec(&frame).map_err(|e| ProtocolError::MalformedFrame {
            detail: format!("json encode: {e}"),
        })?;
        bytes.push(b'\n');
        {
            let mut cached = self
                .shared
                .cached_body
                .lock()
                .map_err(|_| ProtocolError::InternalServerError)?;
            cached.extend_from_slice(&bytes);
        }
        if terminal {
            self.shared.terminal_sent.store(true, Ordering::SeqCst);
            self.shared.complete_if_ready()?;
        }
        self.sender
            .send(StreamingMessage::Frame {
                bytes: Bytes::from(bytes),
                terminal,
            })
            .ok();
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), ProtocolError> {
        Ok(())
    }
}

impl std::fmt::Debug for StreamingFrameWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamingFrameWriter")
            .finish_non_exhaustive()
    }
}

impl Drop for StreamingFrameWriter {
    fn drop(&mut self) {
        if self.shared.terminal_sent.load(Ordering::SeqCst) {
            return;
        }
        let _ = self.sender.send(StreamingMessage::Abort);
        self.shared.clear_active();
    }
}

/// Receiver half for a live operation response stream.
pub struct StreamingBody {
    receiver: UnboundedReceiver<StreamingMessage>,
    shared: Arc<StreamingShared>,
}

impl StreamingBody {
    fn new() -> (StreamingFrameWriter, Self) {
        let (sender, receiver) = unbounded_channel();
        let shared = Arc::new(StreamingShared {
            terminal_sent: AtomicBool::new(false),
            cached_body: Mutex::new(Vec::new()),
            finalizer: Mutex::new(None),
        });
        (
            StreamingFrameWriter {
                sender,
                shared: shared.clone(),
            },
            Self { receiver, shared },
        )
    }

    pub(super) fn set_finalizer(&self, finalizer: StreamingFinalizer) -> Result<(), ProtocolError> {
        {
            let mut current = self
                .shared
                .finalizer
                .lock()
                .map_err(|_| ProtocolError::InternalServerError)?;
            *current = Some(finalizer);
        }
        if self.shared.terminal_sent.load(Ordering::SeqCst) {
            self.shared.complete_if_ready()?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for StreamingBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamingBody").finish_non_exhaustive()
    }
}

enum StreamingMessage {
    Frame { bytes: Bytes, terminal: bool },
    Abort,
}

struct StreamingShared {
    terminal_sent: AtomicBool,
    cached_body: Mutex<Vec<u8>>,
    finalizer: Mutex<Option<StreamingFinalizer>>,
}

impl StreamingShared {
    fn complete_if_ready(&self) -> Result<(), ProtocolError> {
        let finalizer = self
            .finalizer
            .lock()
            .map_err(|_| ProtocolError::InternalServerError)?
            .clone();
        let Some(finalizer) = finalizer else {
            return Ok(());
        };
        let body = self
            .cached_body
            .lock()
            .map_err(|_| ProtocolError::InternalServerError)?
            .clone();
        finalizer.complete(body)
    }

    fn clear_active(&self) {
        if let Ok(finalizer) = self.finalizer.lock()
            && let Some(finalizer) = finalizer.as_ref()
        {
            finalizer.clear_active();
        }
    }
}

#[derive(Clone)]
pub(super) struct StreamingFinalizer {
    pub(super) cache: Arc<Mutex<IdempotencyCache>>,
    pub(super) key: String,
    pub(super) hash: [u8; 32],
    pub(super) response: OperationResponse,
}

impl StreamingFinalizer {
    fn complete(&self, body: Vec<u8>) -> Result<(), ProtocolError> {
        let cached = CachedResponse {
            response: self.response.clone(),
            body,
        };
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| ProtocolError::InternalServerError)?;
        cache.complete(&self.key, self.hash, cached);
        Ok(())
    }

    fn clear_active(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear_active(&self.key, self.hash);
        }
    }
}

pub(super) fn operation_response(
    response: &OperationResponse,
    body_bytes: &[u8],
) -> Response<ResponseBody> {
    let Ok(resp_bytes) = serde_json::to_vec(&response) else {
        return plain_status(StatusCode::INTERNAL_SERVER_ERROR, "encode failed");
    };
    let mut combined = resp_bytes;
    combined.push(b'\n');
    combined.extend_from_slice(body_bytes);
    let body = Full::new(Bytes::from(combined))
        .map_err(|never: Infallible| match never {})
        .boxed();
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/x-ndjson")
        .body(body)
        .unwrap_or_else(|_| plain_status(StatusCode::INTERNAL_SERVER_ERROR, "build failed"))
}

pub(super) fn operation_streaming_response(
    response: &OperationResponse,
    body: StreamingBody,
) -> Response<ResponseBody> {
    let Ok(mut response_line) = serde_json::to_vec(response) else {
        return plain_status(StatusCode::INTERNAL_SERVER_ERROR, "encode failed");
    };
    response_line.push(b'\n');
    let body = LiveOperationBody {
        response_line: Some(Bytes::from(response_line)),
        streaming: body,
        aborted: false,
    }
    .boxed();
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/x-ndjson")
        .body(body)
        .unwrap_or_else(|_| plain_status(StatusCode::INTERNAL_SERVER_ERROR, "build failed"))
}

struct LiveOperationBody {
    response_line: Option<Bytes>,
    streaming: StreamingBody,
    aborted: bool,
}

impl Body for LiveOperationBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if let Some(response_line) = self.response_line.take() {
            return Poll::Ready(Some(Ok(Frame::data(response_line))));
        }
        if self.aborted {
            return Poll::Ready(None);
        }

        match self.streaming.receiver.poll_recv(cx) {
            Poll::Ready(Some(StreamingMessage::Frame { bytes, terminal })) => {
                if terminal {
                    let _ = self.streaming.shared.complete_if_ready();
                }
                Poll::Ready(Some(Ok(Frame::data(bytes))))
            }
            Poll::Ready(Some(StreamingMessage::Abort)) => {
                self.streaming.shared.clear_active();
                self.aborted = true;
                Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"worker aborted")))))
            }
            Poll::Ready(None) => {
                if !self.streaming.shared.terminal_sent.load(Ordering::SeqCst) {
                    self.streaming.shared.clear_active();
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.aborted
    }
}

fn plain_status(status: StatusCode, msg: &'static str) -> Response<ResponseBody> {
    let body = Full::new(Bytes::from_static(msg.as_bytes()))
        .map_err(|never: Infallible| match never {})
        .boxed();
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain")
        .body(body)
        .unwrap_or_else(|_| {
            let fallback = Full::new(Bytes::from_static(b"internal"))
                .map_err(|never: Infallible| match never {})
                .boxed();
            Response::new(fallback)
        })
}
