# Bounded worker HTTP request ingestion (#768)

## Problem and authority

The shared worker receiver collects the whole request before its 1048576-byte
check. #768 requires an ingestion bound, including unknown-length/chunked input.
Scope token q768-e20c181b and the operator-approved design on 2026-10-28 authorize
the behavior below; ADR 0112 records the alternatives and consequences.

## Design

Keep `read_body` private in `http/server.rs`, generic over an unpinned
`http_body::Body<Data = Bytes>` whose error implements Display, so synthetic
frames can exercise the production algorithm without a new transport framework.
Use BodyExt::frame, inspecting data frames before appending to BytesMut.
Ignore trailers/non-data frames and accept empty data frames compatibly.
If `data.len() > MAX_BODY_BYTES - accumulated.len()`, immediately return
FrameTooLarge, with bytes equal to accumulated length plus crossing frame length
and max 1048576. Never append that frame or poll again. Freeze accumulated data
at EOF. No Content-Length or size-hint shortcut.

Transport errors observed before overflow retain InvalidPayload and the
`body collect: …` detail prefix. Oversize retains HTTP 400/FRAME_TOO_LARGE;
bytes now describes an observed prefix, not an unread eventual total.
Route → version → auth → body → endpoint stays intact for all three routes.
Operations retain idempotency header → body-key rejection → hash/decode →
cache/handler. Rejected bodies cannot invoke handlers or claim cache entries.
No ownership transition or caller migration is required: the shared HTTP server
already owns request ingestion. No public contract shape or signature changes.

## Failure model

- Actors and deployments: existing worker HTTP clients, including unauthenticated
  handshake/identity callers and credential-checked operations callers; unknown-size
  bodies and unfinished HTTP/1.1 chunked requests are included.
- Invariants and assets: bounded retained payload, existing route/validation order,
  error mapping, and operation idempotency/handler isolation.
- Accepted failure classes: the transport can supply a frame larger than the limit;
  this loop bounds retained accumulation and polling, not transport-internal buffers.
  No aggregate process-memory or request-duration bound is promised.
- Covered elsewhere: connection quotas/timeouts, response streaming and broader
  transport redesign are excluded and owned by the campaign operator; existing
  handshake authentication work #221 is distinct.

## Threat model

- Boundary inventory: existing network-body → private receiver → endpoint decoding;
  no added or widened route, authentication or public API boundary.
- Actor model: untrusted peers control request bytes on handshake/identity and,
  after existing version/credential checks, operations. Transport framing is supplied
  by hyper; no trust is placed in Content-Length or size hints for admission.
- Control per boundary: preserve route/version/auth before ingestion; reject the first
  oversized data frame before append; retain JSON/endpoint/idempotency validation
  afterward. Errors disclose the existing code/max and approved observed-prefix count
  or existing transport-error detail prefix, never body contents added by this change.
- Explicitly out of scope: aggregate connection pressure, body deadlines, response
  streaming and broader transport/auth redesign, per the approved exclusions and #221.

## Success and validation

Counted synthetic bodies with unknown SizeHint stop on the first crossing frame
in a 4 MiB stream, leaving a substantial tail unread. A late transport error is
never polled; an earlier error keeps its current mapping. Exercise empty, below,
exact, one-over, crossing, huge single frames, trailers and empty data frames.
An actual chunked request crossing the limit receives 400 FRAME_TOO_LARGE before
the terminating chunk. All three routes reject oversize; whitespace-padded valid
JSON of exactly 1 MiB reaches existing validation. Invalid version/auth retain
precedence; a rejected operation leaves handler count and idempotency cache clear.
Focused red/green evidence precedes implementation. Full `just ci` follows final
review and simplification; existing optional ignored tests are disclosed.
