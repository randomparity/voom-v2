# Issue 231 — Worker-protocol version contract: exact-match, no skew window

## Context

Issue #231 (spun off #220/M4 review) reports that the worker wire protocol
encodes **two different version contracts** on its two paths:

- `negotiate` (the `POST /v1/handshake` path,
  `crates/voom-worker-protocol/src/wire/handshake.rs`) accepts **range
  membership** — `offered ∈ [PROTOCOL_VERSION_SUPPORTED_MIN,
  PROTOCOL_VERSION_SUPPORTED_MAX]`.
- `enforce_version` (the `POST /v1/operations` middleware,
  `crates/voom-worker-protocol/src/http/server.rs`) accepts only an **exact**
  match against `PROTOCOL_VERSION`, while building the *same*
  `UnsupportedProtocolVersion` error that advertises a `supported_min..max`
  range it does not honour.

Today `PROTOCOL_VERSION = MIN = MAX = 1`, so the inconsistency is latent: every
live caller offers exactly `PROTOCOL_VERSION`. The chosen direction and rejected
alternatives are recorded in
[ADR 0016](../../adr/0016-worker-protocol-exact-version-match.md). This spec is
the buildable design.

The decision (ADR 0016): **exact-match is the contract**, the range is deleted,
and the two paths share **one** definition of the check so they cannot drift.
The deciding fact is that workers are bundled and lock-stepped with the
control-plane build (ADR 0002; the "no version skew to tolerate" comment in
`operations/transcode_video.rs`), so a `[MIN, MAX]` window is a phantom
capability.

## The single contract

A worker's offered protocol version is **acceptable iff it equals
`voom_core::PROTOCOL_VERSION`**. There is no tolerated neighbourhood. `negotiate`
is the *sole* definition of this check; `enforce_version` reuses it.

```
negotiate(offered):
    offered == PROTOCOL_VERSION
        -> Ok(HandshakeResponse { agreed: offered })
        -> Err(UnsupportedProtocolVersion { offered, expected: PROTOCOL_VERSION })

enforce_version(headers):
    parse x-voom-protocol-version
        absent / unparseable -> Err(InvalidPayload { detail: "missing x-voom-protocol-version" })
        Some(n)              -> negotiate(n).map(drop)   // same contract, reused
```

`agreed` always equals `offered` on success (success *means* equality), so the
`HandshakeResponse` shape and its round-trip tests are unchanged.

## Source changes

Drive each change test-first (per `superpowers:test-driven-development` and
AGENTS.md Rule 9). Files:

1. **`crates/voom-core/src/lib.rs`** — delete `PROTOCOL_VERSION_SUPPORTED_MIN`
   and `PROTOCOL_VERSION_SUPPORTED_MAX`. Rewrite the `PROTOCOL_VERSION` doc
   comment to state the exact-match contract: worker and control plane are
   co-released and version-locked; a worker offering any other version is
   rejected at the handshake; skew is rejected by design; reference ADR 0016.

2. **`crates/voom-worker-protocol/src/wire/envelope.rs`** — change the variant to
   `UnsupportedProtocolVersion { offered: u32, expected: u32 }` and update the
   `#[error("…")]` string to
   `"unsupported protocol version: offered={offered}, expected {expected}"`.

3. **`crates/voom-worker-protocol/src/wire/handshake.rs`** — `negotiate` compares
   `offered == voom_core::PROTOCOL_VERSION`; on mismatch returns
   `UnsupportedProtocolVersion { offered, expected: voom_core::PROTOCOL_VERSION }`.
   Update the doc comment to describe exact-match, not a range.

4. **`crates/voom-worker-protocol/src/http/server.rs`** — `enforce_version`
   delegates: `Some(n) => negotiate(n).map(|_| ())`. Keep the `None =>
   InvalidPayload` arm. Remove the inline `supported_min`/`supported_max`
   construction. (`negotiate` is already imported in this module.)

5. **`crates/voom-fakes/src/bin/chaos_worker.rs`** — the fake's `handle_handshake`
   already exact-matches (`offered == PROTOCOL_VERSION`); update its error
   construction to `{ offered, expected: PROTOCOL_VERSION }`.

6. **`crates/voom-worker-protocol/src/operations/transcode_video.rs`** — append a
   cross-reference to ADR 0016 on the existing "no version skew to tolerate"
   comment so the durable-replay note and the wire contract point at one another.

## Test changes

- **`handshake_test.rs`** — replace the `negotiate_below_min_rejects` /
  `negotiate_above_max_rejects` pair with exact-match coverage: one **inside**
  the window (`negotiate(PROTOCOL_VERSION)` → `agreed == PROTOCOL_VERSION`) and
  one **outside** (`negotiate(PROTOCOL_VERSION + 1)` → `UnsupportedProtocolVersion
  { offered: 2, expected: 1 }`). Drive the constants off
  `voom_core::PROTOCOL_VERSION` where it keeps the test honest, but the matched
  literal must reflect the real field. Satisfies the acceptance criterion ("a
  test covers an offered version inside and outside the supported window").
- **`envelope_test.rs`** — update the `UnsupportedProtocolVersion` construction
  to `{ offered, expected }` and assert the new serialized JSON shape
  (`code`, `offered`, `expected`; no `supported_min`/`supported_max`).
- **`enforce_version` unit coverage (new sibling file).** `enforce_version` has
  **no** unit test today — there is no `crates/voom-worker-protocol/src/http/`
  `*_test.rs` sibling — and the prior coverage audit
  (`docs/test-coverage-audit-2026-05-28.md`) records the missing-header →
  `InvalidPayload` branch as pinned by nothing (the conformance probe loosely
  accepts either variant). That branch is `enforce_version`'s **own** logic, not
  reached through `negotiate`, so pin it. Add
  `crates/voom-worker-protocol/src/http/server_test.rs` and wire it from
  `server.rs` with the mandatory sibling-test declaration (AGENTS.md test layout /
  ADR-0004):

  ```rust
  // at the bottom of server.rs
  #[cfg(test)]
  #[path = "server_test.rs"]
  mod tests;
  ```

  `enforce_version` is private; the sibling test reaches it via `use super::*;`
  (no visibility change). Two cases, both building a `hyper::HeaderMap`:
  - present-but-wrong version header (`PROTOCOL_VERSION + 1`) → `Err(`
    `UnsupportedProtocolVersion { offered, expected })` (proves the delegation to
    `negotiate` carries through);
  - **no** `x-voom-protocol-version` header → `Err(InvalidPayload { .. })` (pins
    the previously-uncovered branch).

  Do **not** add an inline `#[cfg(test)] mod tests { … }` to `server.rs`:
  `just check-test-layout` (in `just ci`) rejects inline test modules in `src/`.
- **`crates/voom-conformance/src/typed_suite.rs`** — the test is referenced at
  **three** sites that all must move together: the case registration display
  string (`typed_suite.rs:88`, the `format!("…::handshake_rejects_below_-`
  `supported_min", …)` literal — the compiler will **not** flag a stale string),
  the call in that same registration block (`:89`), and the function definition
  (`:199`, which offers `PROTOCOL_VERSION_SUPPORTED_MIN - 1`). Rename all three to
  `handshake_rejects_unsupported_version` and offer a non-matching version
  (`PROTOCOL_VERSION + 1`). The body already asserts
  `matches!(e, UnsupportedProtocolVersion { .. })`, which still holds.
- **`crates/voom-conformance/src/raw_wire_suite.rs`** — the
  `UnsupportedProtocolVersion { .. }` match arm is field-agnostic and unaffected,
  and the error is decoded by `RawHttpResponse::protocol_error()` via a plain
  `serde_json::from_slice` into `ProtocolError` (not a key-by-key assertion), so
  the `{offered, expected}` reshape needs **no** decoder change — confirmed no
  conformance test asserts the old `supported_min`/`supported_max` keys. Confirm
  the offered version it sends (`{"offered": 0}`) still triggers the error.

## Verification

`just ci` (`fmt-check`, `lint`, `check-test-layout`, `test`, `doc`, `deny`,
`audit`) green. The new behavior is fully covered by the workspace unit tests and
the conformance suite — no hardware or external services required. `just doc`
must stay clean (the rewritten doc comments and the removed constants must not
leave dangling intra-doc links).

## Out of scope / non-goals

- **Re-introducing a real multi-version window.** Deliberately deferred to when a
  non-lock-stepped worker exists (ADR 0016 Consequences).
- **Historical sprint docs** under `docs/superpowers/specs/` and `plans/` that
  describe the original range design. They are point-in-time records; ADR 0016
  supersedes them. Not rewritten (append-only history).
- **The `agreed` field / `HandshakeResponse` shape.** Unchanged.
