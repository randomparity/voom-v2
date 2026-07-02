# Spec: NDJSON reader bounds the frame before buffering it (issue #258)

Status: draft
Date: 2026-07-01
Issue: #258
Base ref audited: `d2bc28e`

## Context

`NdjsonReader::next_frame` reads a whole line with
`BufReader::read_until(b'\n', &mut self.line_buf)`
(`crates/voom-worker-protocol/src/wire/ndjson.rs:103`) and only *then* checks
`payload_len > self.max_frame_bytes` (`:122`). `read_until` has no size cap, so a
worker that emits bytes without a newline forces the entire line into `line_buf`
before `FrameTooLarge` can fire. `max_frame_bytes` (64 KiB default) therefore
bounds the *rejection*, not the *allocation*.

The trust model is loopback-only, co-released workers (the control plane is the
trust root), so this is defense-in-depth, not a remote-exploitable hole. But the
module already documents the size contract — "A single line longer than
`max_frame_bytes` (default 64 KiB) aborts the stream with `FrameTooLarge`"
(`ndjson.rs:15`) — and today that contract is not enforced against memory. A
buggy or hostile worker stuck emitting a multi-GB line with no newline OOMs the
control plane.

This spec conforms to the **existing** framing contract; it does not introduce a
new architectural decision (see "Decisions"). It also removes one dead field the
issue flags as a related LOW finding.

## Item 1 — Cap the read at the frame bound (primary finding)

`crates/voom-worker-protocol/src/wire/ndjson.rs`

**Current:** `read_until` buffers the entire line, then `:122` rejects it. Memory
is unbounded until EOF or newline.

**Target:** Read the line through the buffered reader in chunks
(`AsyncBufReadExt::fill_buf` / `consume`), appending to `line_buf` and stopping as
soon as the accumulated bytes exceed `max_frame_bytes`, returning `FrameTooLarge`
at that point instead of after buffering the whole line. A chunk that contains the
terminating newline is appended up to and including the newline. `BufReader`'s
internal buffer bounds each chunk, so `line_buf` can overshoot the bound by at
most one buffer fill before the check fires — memory is bounded to
`max_frame_bytes + BufReader_capacity`, not the line length.

All other framing semantics are preserved unchanged:

- Clean EOF on a frame boundary (nothing buffered) → `StreamEnd`.
- EOF with a partial line buffered (no trailing newline) → `MalformedFrame`
  ("stream truncated mid-frame").
- A complete line → strip the trailing newline, enforce the size bound, decode,
  then run the existing lease / seq / terminal logic.
- Duplicate / lower-seq frames are still dropped in the outer `loop` with
  `continue` (the M11 fix), reading the next line without recursion.

**Failure-contract nuance (`FrameTooLarge.bytes`):** Today `bytes` is the exact
payload length of the offending line. Under the cap the reader must reject before
reading the whole line, so `bytes` becomes "the bytes buffered at the point the
bound was exceeded" — always `> max`, but no longer necessarily the true line
length for a streaming source. This is intended: computing the true length would
require reading the whole line, defeating the fix. `bytes` remains a
diagnostic-only field of `ProtocolError::FrameTooLarge`; only `ProtocolError`
`code` strings are public contract (they are unchanged). `BufReader::fill_buf`
returns at most the reader's internal capacity per fill (tokio default 8 KiB),
regardless of the underlying source, so `bytes` equals the exact payload length
only when the offending line fits within a single fill. The existing
`frame_too_large_rejects` test (a 201-byte `&[u8]` line, well under 8 KiB) is one
such case and is unaffected; a hypothetical over-length line exceeding one fill
would report a capped `bytes` value.

**Edge cases:**
- Over-length line from a chunked stream → `FrameTooLarge` with `bytes > max`,
  and the reader consumes at most `max + BufReader_capacity` bytes of input
  before erroring (the load-bearing new behavior).
- Over-length line from an in-memory `&[u8]` that fits within one `fill_buf`
  (≤ capacity) → `FrameTooLarge` with `bytes` equal to the exact payload length
  (unchanged; covers the existing 201-byte test).
- Line exactly `max_frame_bytes` payload + newline → accepted (boundary,
  unchanged).
- Empty line (`"\n"`) → payload length 0 → serde decode error →
  `MalformedFrame` (unchanged).
- EOF immediately (nothing buffered) → `StreamEnd` (unchanged).
- EOF after a partial line → `MalformedFrame` "truncated" (unchanged).
- Read error from the underlying reader → `MalformedFrame` "read error"
  (unchanged).

**Acceptance criteria:**
- A reader that streams `N` non-newline bytes (`N` fixed at 4 MiB — far larger
  than `max_frame_bytes`, yet small enough to fully allocate in the pre-fix red
  run) in bounded chunks returns `FrameTooLarge` **and** consumes far fewer than
  `N` bytes, proven by a counting reader that records total bytes served. Assert
  `served` is orders of magnitude below `N` (e.g. `served < 64 KiB`) rather than a
  tight `max + capacity`, so the guarantee tested is "bounded, not
  line-proportional" and the test does not depend on the undocumented BufReader
  capacity constant. Against the pre-fix code this test fails (all `N` bytes are
  consumed); against the fix it passes.
- The existing `frame_too_large_rejects` test (in-memory `&[u8]`,
  `bytes: 200, max: 64`) still passes unchanged.
- All existing `ndjson_test.rs` cases still pass (seq monotonicity, duplicate
  drop, wrong lease, terminal, EOF/StreamEnd, mid-frame malformed, malformed
  JSON, writer tests).

## Item 2 — Remove the always-zero `StreamEnd { partial_bytes }` field (related LOW)

`crates/voom-worker-protocol/src/wire/ndjson.rs` and its consumers.

**Current:** `NdjsonOutcome::StreamEnd { partial_bytes: usize }` is documented as
recording bytes accumulated before EOF, but it is structurally always `0`: a clean
close returns `partial_bytes: 0` (`:111`) and a partial trailing line takes the
`MalformedFrame` branch (`:129-139`) instead, so the non-zero state is
unreachable. Every consumer already ignores it with `{ .. }` — production sites in
`voom-control-plane` (`worker_process.rs`, `workflow/execution/dispatch.rs`) plus
match arms across the `voom-conformance` and `voom-fakes` test harnesses and
`http_test.rs`. The field and its doc describe an unreachable state.

**Target:** Make `StreamEnd` a unit variant (`NdjsonOutcome::StreamEnd`). Update
the enum, the return site, the module/method docs, the sibling test, and every
consumer match arm (`{ .. }` / `{ partial_bytes: 0 }` → bare `StreamEnd`). The
compiler enumerates the arms authoritatively — a `StreamEnd { .. }` struct pattern
against a unit variant is a compile error — so a clean workspace build is the
completeness proof (see acceptance criteria).

**Rationale / safety:** `NdjsonOutcome` derives only
`Debug, Clone, PartialEq, Eq` — it is never serialized to the DB or the wire, so
this is an in-process API change, **not** a durable-payload change under ADR-0013.
The removed state was already unreachable, so no behavior changes.

**Acceptance criteria:**
- `just test` (workspace, `--all-features`) compiles and passes — the compiler is
  the authoritative completeness signal, because a surviving
  `NdjsonOutcome::StreamEnd { .. }` struct-pattern arm fails to compile against a
  unit variant (a `partial_bytes` grep would not catch it, since such an arm names
  no field).
- `rg 'StreamEnd \{' crates/` and `rg partial_bytes crates/` both return nothing.

## Decisions

- **No new ADR.** The fix makes the reader enforce the byte cap already
  documented in the NDJSON module contract (`ndjson.rs:15`) and the Sprint-2
  Phase-1 design; it changes no layer boundary, interface split, concurrency
  invariant, or migration. This mirrors the #261 precedent (defense-in-depth
  cleanup conforming to an existing contract adds no ADR). The `FrameTooLarge.bytes`
  semantic refinement and the `partial_bytes` removal are recorded here rather
  than in a standalone ADR.
- **`NdjsonOutcome` is not a durable payload.** It has no `Serialize`/`Deserialize`
  derive and is consumed only in-process, so removing `partial_bytes` is outside
  the ADR-0013 additive-evolution contract.
- **Direct implementation, not subagent fan-out.** Items 1 and 2 are one tightly
  coupled unit in a single crate (the read-cap rewrite and the enum change touch
  the same function and type). Direct TDD in this session is the right execution
  mode; no parallel mutating agents.

## Plan (TDD, direct implementation)

Guardrails before each commit: `just fmt-check`, `just lint`,
`cargo test -p voom-worker-protocol`; `just ci` before pushing.

1. **Failing test — bounded consumption.** In `ndjson_test.rs`, add a `CountingReader`
   `AsyncRead` that serves `N = 4 MiB` of non-newline bytes in bounded chunks and
   records total bytes served. Test: with a small `max_frame_bytes`, `next_frame`
   returns `FrameTooLarge` and `served` is far below `N` (assert `served < 64 KiB`).
   Confirm it fails against the current `read_until` implementation (all `N` bytes
   consumed).
2. **Implement the cap.** Replace the `read_until` call with a chunked read helper
   (`fill_buf`/`consume`) that appends to `line_buf` and returns `FrameTooLarge` once
   the accumulated bytes exceed `max_frame_bytes`; return whether a newline
   terminated the line. Reshape `next_frame` to branch on (buffered-empty → EOF
   `StreamEnd`) / (no newline → `MalformedFrame`) / (newline → existing decode path).
   Keep the outer duplicate-drop `loop`.
3. **Green.** Run the new test and the full `ndjson_test.rs` suite; confirm the
   bounded-consumption test passes and every prior case still passes.
4. **Remove `partial_bytes`.** Make `StreamEnd` a unit variant; update the enum,
   both return sites, docs, the sibling test, and the three consumer match arms.
   Update the module doc line describing `StreamEnd`.
5. **Guardrails.** `just lint` (clippy pedantic, `-D warnings`), `just fmt-check`,
   `just test` (workspace), `just doc`. Fix every warning.

## Out of scope

- The `read_body` size guard in `http/server.rs:408` is a separate request-body
  path bounded by hyper's collector; it is not the streaming NDJSON reader and is
  not touched.
- No change to `max_frame_bytes` default (64 KiB) or the `with_max_frame_bytes`
  builder.
