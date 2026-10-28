use super::*;

#[test]
fn enforce_version_wrong_version_rejects() {
    let mut headers = hyper::HeaderMap::new();
    headers.insert(
        HeaderName::from_static(PROTOCOL_VERSION_HEADER),
        hyper::header::HeaderValue::from_str(&(voom_core::PROTOCOL_VERSION + 1).to_string())
            .unwrap(),
    );
    let err = enforce_version(&headers).unwrap_err();
    assert!(matches!(
        err,
        ProtocolError::UnsupportedProtocolVersion {
            offered,
            expected,
        } if offered == voom_core::PROTOCOL_VERSION + 1
            && expected == voom_core::PROTOCOL_VERSION
    ));
}

#[test]
fn identity_route_is_read_only_and_unauthenticated() {
    assert_eq!(
        route_policy(&Method::POST, "/v1/identity"),
        Some(RoutePolicy {
            version: false,
            auth: false,
        })
    );
}

#[test]
fn enforce_version_missing_header_is_invalid_payload() {
    let headers = hyper::HeaderMap::new();
    let err = enforce_version(&headers).unwrap_err();
    assert!(matches!(
        &err,
        ProtocolError::InvalidPayload { detail } if detail.contains("missing")
    ));
}

#[test]
fn enforce_version_malformed_header_reports_malformed_not_missing() {
    let mut headers = hyper::HeaderMap::new();
    headers.insert(
        HeaderName::from_static(PROTOCOL_VERSION_HEADER),
        hyper::header::HeaderValue::from_static("1.0"),
    );
    let err = enforce_version(&headers).unwrap_err();
    assert!(
        matches!(
            &err,
            ProtocolError::InvalidPayload { detail }
                if detail.contains("malformed") && detail.contains("1.0") && !detail.contains("missing")
        ),
        "expected a malformed-value InvalidPayload, got {err:?}"
    );
}

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
