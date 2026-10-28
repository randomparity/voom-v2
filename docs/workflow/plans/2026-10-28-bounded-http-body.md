# Implement bounded worker HTTP ingestion (#768)

Goal: stop ingestion at the first oversized frame while preserving existing validation.
Architecture: the existing private HTTP receiver remains the owner; no caller migration.
Stack: existing bytes/http-body/http-body-util/hyper/tokio dependencies only.
Expected implementation size: 250–420 changed lines (M) — receiver ~30, counted tests ~140, HTTP tests ~150 plus formatting.
Branch: feat/bounded-http-body-768. BASE_BRANCH: main.
Scope: q768-e20c181b; full-spec, fixed M denominator 250 from live #768 assessment
on 033729e8 in WORK:SCOPE. Plan estimate is informational, no scope delta.

## Global Constraints

Keep route → version → auth → body → endpoint ordering and operation
idempotency-header → body-key → hash/decode → cache/handler ordering.
No public contract shape or signature changes.
Connection quotas/timeouts, response streaming and broader transport redesign
are excluded and owned by the campaign operator. Existing handshake authentication
work #221 is distinct. No claim of an aggregate process-memory or duration bound.
Use repository Rust 1.95 and existing dependencies; no new version floors or pins.
Sibling source tests; real-time transport fixtures; CARGO_BUILD_JOBS=2.
Guardrails: focused cargo tests, just fmt-check, mandatory pre-commit hooks,
assembled crate test, and final just ci owned by this quest before push.

## Task 1: counted-body red proof

Modify `crates/voom-worker-protocol/src/http/server_test.rs`: append the complete
code below. In server.rs change only read_body's private signature to the Task 2
generic signature (keep collect body) so this proof can drive production code.
Interfaces: CountedBody implements Body<Data=Bytes, Error=&'static str> with the
trait's default unknown SizeHint; counted_body takes frame results and returns it;
data_frame takes usize and returns Frame<Bytes>.
Verification — Mode: focused-test. Contract: no polling beyond first overflow,
exact observed-prefix diagnostic, before/after-overflow error ordering.
Run `cargo test -p voom-worker-protocol read_body_` before the loop: expected red,
post-collect reports 4 MiB/consumes tail or observes unread tail error. Boundary
cases already supported may pass; do not claim their unchanged behavior was red.
Green same command after Task 2: three named tests below pass.

```rust
struct CountedBody {
    frames: std::collections::VecDeque<Result<http_body::Frame<Bytes>, &'static str>>,
    polls: Arc<std::sync::atomic::AtomicUsize>,
    served: Arc<std::sync::atomic::AtomicUsize>,
}

impl http_body::Body for CountedBody {
    type Data = Bytes;
    type Error = &'static str;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        use std::sync::atomic::Ordering;
        self.polls.fetch_add(1, Ordering::SeqCst);
        let frame = self.frames.pop_front();
        if let Some(Ok(frame)) = &frame
            && let Some(data) = frame.data_ref()
        {
            self.served.fetch_add(data.len(), Ordering::SeqCst);
        }
        std::task::Poll::Ready(frame)
    }
}

fn counted_body(frames: Vec<Result<http_body::Frame<Bytes>, &'static str>>) -> CountedBody {
    CountedBody {
        frames: frames.into(),
        polls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        served: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    }
}

fn data_frame(len: usize) -> http_body::Frame<Bytes> {
    http_body::Frame::data(Bytes::from(vec![b'x'; len]))
}

#[tokio::test]
async fn read_body_stops_at_crossing_frame_without_consuming_unknown_size_tail() {
    use std::sync::atomic::Ordering;
    // The unread 2.75 MiB tail is the resource-bound assertion: post-collection
    // validation would consume it and report 4 MiB instead of the observed prefix.
    let body = counted_body(
        (0..16)
            .map(|_| Ok(data_frame(MAX_BODY_BYTES / 4)))
            .collect(),
    );
    assert_eq!(http_body::Body::size_hint(&body).upper(), None);
    let polls = body.polls.clone();
    let served = body.served.clone();
    let observed = MAX_BODY_BYTES + MAX_BODY_BYTES / 4;
    assert_eq!(
        read_body(body).await.unwrap_err(),
        ProtocolError::FrameTooLarge {
            bytes: observed as u64,
            max: MAX_BODY_BYTES as u64,
        }
    );
    assert_eq!(polls.load(Ordering::SeqCst), 5);
    assert_eq!(served.load(Ordering::SeqCst), observed);
}

#[tokio::test]
async fn read_body_preserves_boundaries_and_ignores_non_data_frames() {
    for sizes in [
        vec![],
        vec![0],
        vec![3],
        vec![MAX_BODY_BYTES - 1],
        vec![MAX_BODY_BYTES],
    ] {
        let expected: usize = sizes.iter().sum();
        let body = counted_body(sizes.into_iter().map(|len| Ok(data_frame(len))).collect());
        assert_eq!(read_body(body).await.unwrap(), vec![b'x'; expected]);
    }
    let body = counted_body(vec![
        Ok(data_frame(2)),
        Ok(data_frame(0)),
        Ok(http_body::Frame::trailers(hyper::HeaderMap::new())),
    ]);
    assert_eq!(read_body(body).await.unwrap(), &b"xx"[..]);
    for sizes in [
        vec![MAX_BODY_BYTES + 1],
        vec![MAX_BODY_BYTES - 1, 2],
        vec![4 * MAX_BODY_BYTES],
    ] {
        let observed: usize = sizes.iter().sum();
        let body = counted_body(sizes.into_iter().map(|len| Ok(data_frame(len))).collect());
        assert_eq!(
            read_body(body).await.unwrap_err(),
            ProtocolError::FrameTooLarge {
                bytes: observed as u64,
                max: MAX_BODY_BYTES as u64,
            }
        );
    }
}

#[tokio::test]
async fn read_body_reports_only_transport_errors_observed_before_overflow() {
    use std::sync::atomic::Ordering;
    let body = counted_body(vec![Ok(data_frame(MAX_BODY_BYTES)), Err("early transport")]);
    assert_eq!(
        read_body(body).await.unwrap_err(),
        ProtocolError::InvalidPayload {
            detail: "body collect: early transport".to_owned(),
        }
    );
    let body = counted_body(vec![
        Ok(data_frame(MAX_BODY_BYTES)),
        Ok(data_frame(1)),
        Err("unread tail"),
    ]);
    let polls = body.polls.clone();
    assert_eq!(
        read_body(body).await.unwrap_err(),
        ProtocolError::FrameTooLarge {
            bytes: MAX_BODY_BYTES as u64 + 1,
            max: MAX_BODY_BYTES as u64,
        }
    );
    assert_eq!(
        polls.load(Ordering::SeqCst),
        2,
        "a tail error must remain unobserved"
    );
}
```

## Task 2: replace collection with bounded ingestion

Modify only read_body in `crates/voom-worker-protocol/src/http/server.rs`.
Interfaces: private async read_body<B>(B) -> Result<Bytes, ProtocolError>,
B: Body<Data=Bytes> + Unpin, B::Error: Display; existing Incoming caller unchanged.
Verification — Mode: focused-test. First-crossing/tail/error contracts use Task 1's
red evidence; same exact green command must pass all three named read_body_ tests.
Boundary test covers empty/below/exact/one-over/crossing/huge/trailers/empty frames.
Replace the full helper with:

```rust
async fn read_body<B>(mut body: B) -> Result<Bytes, ProtocolError>
where
    B: http_body::Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Display,
{
    let mut bytes = bytes::BytesMut::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| ProtocolError::InvalidPayload {
            detail: format!("body collect: {e}"),
        })?;
        if let Ok(data) = frame.into_data() {
            if data.len() > MAX_BODY_BYTES - bytes.len() {
                return Err(ProtocolError::FrameTooLarge {
                    bytes: bytes.len() as u64 + data.len() as u64,
                    max: MAX_BODY_BYTES as u64,
                });
            }
            bytes.extend_from_slice(&data);
        }
    }
    Ok(bytes.freeze())
}
```

## Task 3: real transport and compatibility evidence

Append the complete code below to `crates/voom-worker-protocol/src/http_test.rs`.
Interfaces: existing running_server(OperationHandler)->(SocketAddr,ServerRunning),
operation_handler(Arc<AtomicUsize>,Option<LeaseId>)->OperationHandler,
write_chunk(&mut TcpStream,&[u8])->io::Result<()>; worker_post returns reqwest's
RequestBuilder for existing worker headers. Read existing helpers before editing.
Verification — Mode: focused-test. Contract: reject an unfinished chunked request
before EOF. Run `cargo test -p voom-worker-protocol oversized_chunked_body_rejects_before_request_eof`
with old collect behavior: timeout after five seconds is expected red (sender open,
no terminating zero chunk). With Task 2: one test passes, 400 FRAME_TOO_LARGE before EOF.
Temporarily restore only old collect implementation for this red, then restore loop.
Verification — Mode: focused-test. Compatibility contract: all three routes accept
exact-limit padded valid JSON/reject one-over; earlier route/version/auth guards win;
rejected operations never reserve the key or call the handler. Run
`cargo test -p voom-worker-protocol body_limit_preserves_routes_validation_and_idempotency`:
one green test. These unchanged behaviors are not expected red on old code, so
red proof belongs to the central ingestion contract above, not these assertions.

```rust
#[tokio::test]
async fn oversized_chunked_body_rejects_before_request_eof() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, running) = running_server(operation_handler(calls.clone(), None)).await;
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            b"POST /v1/handshake HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
    let chunk = vec![b'x'; 64 * 1024];
    for _ in 0..16 {
        write_chunk(&mut stream, &chunk).await.unwrap();
    }
    stream.write_all(b"1\r\nx\r\n").await.unwrap();
    // Keep the sender open and deliberately omit the terminating zero chunk.
    // Waiting for collection/EOF must fail this bounded wait, not hang the suite.
    let response = tokio::time::timeout(Duration::from_secs(5), async {
        let mut response = Vec::new();
        loop {
            let mut buf = [0_u8; 1024];
            let n = stream.read(&mut buf).await.unwrap();
            assert_ne!(n, 0, "connection ended without a complete JSON error");
            response.extend_from_slice(&buf[..n]);
            if let Some(split) = response.windows(4).position(|w| w == b"\r\n\r\n")
                && let Ok(error) = serde_json::from_slice::<ProtocolError>(&response[split + 4..])
            {
                assert!(response.starts_with(b"HTTP/1.1 400 "));
                return error;
            }
            assert!(response.len() < 4096, "unexpectedly large error response");
        }
    })
    .await
    .expect("oversize must be rejected before request EOF");
    assert!(
        matches!(response, ProtocolError::FrameTooLarge { bytes, max }
        if max == 1 << 20 && bytes > max && bytes == max + 1)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(stream);
    let _ = running.shutdown.send(());
    running.joined.await.unwrap();
}

fn worker_post(client: &reqwest::Client, addr: SocketAddr, route: &str) -> reqwest::RequestBuilder {
    client
        .post(format!("http://{addr}{route}"))
        .header(PROTOCOL_VERSION_HEADER, voom_core::PROTOCOL_VERSION)
        .header(WORKER_ID_HEADER, 7)
        .header(WORKER_EPOCH_HEADER, 3)
        .bearer_auth("secret")
        .header(IDEMPOTENCY_KEY_HEADER, "bounded-body")
}

#[tokio::test]
async fn body_limit_preserves_routes_validation_and_idempotency() {
    const LIMIT: usize = 1 << 20;
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, running) = running_server(operation_handler(calls.clone(), None)).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let oversized = vec![b' '; LIMIT + 1];
    for route in ["/v1/handshake", "/v1/identity", "/v1/operations"] {
        let response = worker_post(&client, addr, route)
            .body(oversized.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: ProtocolError = response.json().await.unwrap();
        assert!(matches!(error, ProtocolError::FrameTooLarge { bytes, max }
            if bytes == LIMIT as u64 + 1 && max == LIMIT as u64));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // Earlier route/version/auth guards keep precedence over oversized bodies.
    for (route, version, secret, status, code) in [
        (
            "/unknown",
            voom_core::PROTOCOL_VERSION,
            "secret",
            StatusCode::NOT_FOUND,
            None,
        ),
        (
            "/v1/operations",
            voom_core::PROTOCOL_VERSION + 1,
            "secret",
            StatusCode::BAD_REQUEST,
            Some("UNSUPPORTED_PROTOCOL_VERSION"),
        ),
        (
            "/v1/operations",
            voom_core::PROTOCOL_VERSION,
            "wrong",
            StatusCode::UNAUTHORIZED,
            Some("UNAUTHORIZED_BEARER"),
        ),
    ] {
        let response = client
            .post(format!("http://{addr}{route}"))
            .header(PROTOCOL_VERSION_HEADER, version)
            .header(WORKER_ID_HEADER, 7)
            .header(WORKER_EPOCH_HEADER, 3)
            .bearer_auth(secret)
            .body(oversized.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        if let Some(code) = code {
            let error: serde_json::Value = response.json().await.unwrap();
            assert_eq!(error["code"], code);
        }
    }
    let operation = serde_json::to_vec(&request(LeaseId(1), serde_json::json!({}))).unwrap();
    for (route, mut body) in [
        (
            "/v1/handshake",
            serde_json::to_vec(&crate::HandshakeRequest {
                offered: voom_core::PROTOCOL_VERSION,
            })
            .unwrap(),
        ),
        (
            "/v1/identity",
            serde_json::to_vec(&crate::WorkerIdentityRequest {
                offered: voom_core::PROTOCOL_VERSION,
                challenge: "00".repeat(16),
            })
            .unwrap(),
        ),
        ("/v1/operations", operation),
    ] {
        body.resize(LIMIT, b' ');
        let response = worker_post(&client, addr, route)
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "exact-limit valid JSON on {route}"
        );
        let _ = response.bytes().await.unwrap();
    }
    // The successful same-key operation proves rejected bodies never reserved it.
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let _ = running.shutdown.send(());
    running.joined.await.unwrap();
}
```

## Integration, review and recovery

Run `just fmt`, `just fmt-check`, then `cargo test -p voom-worker-protocol`:
all selected unit tests pass; five existing opt-in Toxiproxy tests remain ignored.
Review whole branch with failure-injection lens against spec Failure model and
Threat model; independent security pass checks that inventory. Simplify surgically.
Run final `CARGO_BUILD_JOBS=2 just ci`, commit through hooks, deliver under claim
gates and hand off a commit-bound merge-ready PR. Do not merge.
Non-runtime ADR/spec/plan/index: Mode: task-test-not-applicable — prose documents
contracts; validate layout/index and review instead of inventing executable tests.
Rollback is a code revert; no durable state changes. No ownership transition.

## Resume state

Current step: corrected design awaiting scope audit. Claim q768-e20c181b; ADR0112.
Deferrals, follow-up candidates, rejected findings and outstanding notes: none.
Design review findings: explicit threat model, execution-complete plan and bounded
response/exact-prefix assertions accepted and incorporated without scope change.
Final full local verification owner: this quest, just ci before push; pre-commit
runs its configured lighter suite, no installed pre-push hook.
