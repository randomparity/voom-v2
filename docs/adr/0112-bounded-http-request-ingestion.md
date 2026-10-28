# ADR 0112: Bound worker HTTP request ingestion before append

## Status

Accepted (2026-10-28)

## Context

Issue #768 identifies that the worker HTTP server checks its 1 MiB request
limit after `collect()`, leaving ingestion unbounded. The operator approved
frame-by-frame rejection and its diagnostic/failure-order changes on 2026-10-28.
The shared receiver owns this concern for handshake, identity and operations.

## Decision

Keep the existing private `read_body` call site. Poll body frames, ignore
non-data frames, and check each data frame against the remaining 1048576-byte
capacity before appending. Return immediately on the first crossing frame;
do not poll the remaining body. Do not use Content-Length or size hints as
an admission shortcut. At/below-limit bodies continue through existing checks.

Preserve HTTP 400 `FRAME_TOO_LARGE` and `max=1048576`. Its `bytes` is the
observed prefix including the crossing frame, rather than eventual total size.
A transport error observed before overflow remains `INVALID_PAYLOAD` with
`body collect: …`; an unread error after overflow is not observed.
Keep route → version → auth → body → endpoint ordering and operation
idempotency-header → body-key → hash/decode → cache/handler ordering.

## Consequences

The receiver retains at most the limit in accumulated payload; a transport
frame already supplied by the body implementation may itself exceed it.
This is a per-request accumulation bound, not a connection quota or deadline.
No wire shape, public signature, dependency or persistence change is required.
Connection quotas/timeouts, response streaming and broader transport redesign
remain separately scoped work owned by the campaign operator.

## Considered & rejected

- Keep post-collection validation. verified: on 033729e8,
  `sed -n '412,429p' crates/voom-worker-protocol/src/http/server.rs`
  shows `collect().await` before the length check, which cannot stop ingestion.
- Use `http_body_util::Limited`. judgment: retaining the approved observed-prefix
  diagnostic would need extra counting; the local loop is simpler for this contract.
- Check Content-Length only. judgment: insufficient for the required
  unknown-length/chunked case and unnecessary as an additional rejection path.
