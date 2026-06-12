---
status: accepted
date: 2026-06-11
deciders: [VOOM core]
---

# 0016 — Worker protocol enforces an exact version match, no skew window

## Context

The worker wire protocol (`voom-worker-protocol`) carries a version on two
paths. A worker offers a version at the `POST /v1/handshake` negotiation
(`negotiate`), and every `POST /v1/operations` request repeats it in the
`x-voom-protocol-version` header, re-checked by the `enforce_version`
middleware. The version constants live in `voom-core`.

The #220/M4 review (settled in ADR 0013) spun off #231 after noticing the two
paths encoded **two different contracts**:

- `negotiate` accepted **range membership** — any offered version in
  `[PROTOCOL_VERSION_SUPPORTED_MIN, PROTOCOL_VERSION_SUPPORTED_MAX]`.
- `enforce_version` accepted only an **exact** match against
  `PROTOCOL_VERSION`, yet built the *same* `UnsupportedProtocolVersion` error
  advertising a `supported_min..supported_max` range it did not actually honor.

Today `PROTOCOL_VERSION = MIN = MAX = 1`, so the range is a single point and the
divergence is latent — every live caller (`HttpClient`, the conformance suites,
`compliance.rs`) offers exactly `PROTOCOL_VERSION` on both paths. The bug bites
the first time someone widens `MIN`/`MAX` or bumps `PROTOCOL_VERSION` intending a
multi-version window: `negotiate` would accept a neighbouring version that
`enforce_version` then rejects mid-session, and the error payload advertises a
tolerance the operations path never had.

Two architectural facts decide the direction rather than the symptom:

- **Workers are bundled, co-deployed, and lock-stepped with the control-plane
  build.** ADR 0002 makes every provider an out-of-process worker, but the
  built-in workers (`voom-ffprobe-worker`, `voom-ffmpeg-worker`,
  `voom-mkvtoolnix-worker`, `voom-verify-artifact-worker`) ship in the same
  binary set and are launched by the control plane
  (`VOOM_FFMPEG_WORKER_BIN`, …). A worker process is never a different release
  than the control plane that spawns it.
- **The codebase already assumes no skew.**
  `crates/voom-worker-protocol/src/operations/transcode_video.rs` documents the
  request/result schemas as "lock-stepped with the control-plane build … There
  is no version skew to tolerate." A `[MIN, MAX]` window is machinery for a
  cross-version-skew deployment the architecture precludes — a phantom
  capability advertised in an error payload but exercised by nothing.

`#231` asked to settle the contract deliberately, either by honouring the range
or by dropping it for strict exact-match.

Design doc:
[`docs/superpowers/specs/2026-06-11-issue-231-worker-protocol-version-contract-design.md`](../superpowers/specs/2026-06-11-issue-231-worker-protocol-version-contract-design.md).

## Decision

Adopt **exact-match** as the single worker-skew contract and delete the range.

1. **One contract, one definition.** A worker's offered version is acceptable
   iff it equals `voom_core::PROTOCOL_VERSION`. `negotiate(offered)` is the sole
   definition of that check; the `enforce_version` middleware **delegates to
   `negotiate`** rather than re-implementing the comparison, so the handshake and
   the per-request gate cannot drift apart again. `enforce_version` keeps its own
   `InvalidPayload` result for a *missing* header (a malformed request, distinct
   from an unsupported version).

2. **Drop the range from the type system.** Remove
   `PROTOCOL_VERSION_SUPPORTED_MIN` and `PROTOCOL_VERSION_SUPPORTED_MAX` from
   `voom-core`. Re-shape the wire error from
   `UnsupportedProtocolVersion { offered, supported_min, supported_max }` to
   `UnsupportedProtocolVersion { offered, expected }`, where `expected` is the
   single `PROTOCOL_VERSION` the server speaks. The diagnostic stays useful (a
   rejected worker learns which version to be) without implying a tolerated
   window.

3. **Document the contract where the constant lives.** The `PROTOCOL_VERSION`
   doc comment in `voom-core` states the exact-match rule and that skew is
   rejected by design; `transcode_video.rs`'s existing "no version skew" comment
   cross-references this ADR. Bumping `PROTOCOL_VERSION` is therefore a
   binary-before-nothing flag day: all workers and the control plane move
   together because they are the same release.

This is a pre-release, wire-visible contract change (ADR-aligned with AGENTS.md
Rule 1, architectural correctness over compatibility shims). The
`UnsupportedProtocolVersion` JSON body changes from `{offered, supported_min,
supported_max}` to `{offered, expected}`; there is no external consumer to
migrate because workers are co-released.

## Consequences

- The handshake and the operations gate provably agree: there is one comparison,
  reused. A future `PROTOCOL_VERSION` bump cannot reintroduce the split because
  there is no second copy to forget.
- The error payload no longer claims a skew tolerance the system does not have.
  A version mismatch fails loud and early at the handshake (AGENTS.md Rule 12),
  with the exact `expected` version, before any operation dispatches.
- Re-introducing a real multi-version window later is a deliberate change: it
  must re-add a range type, teach `negotiate` to pick an `agreed` version that
  differs from `offered`, and re-verify that the durable-replay assumptions in
  `transcode_video.rs` still hold. That is the right place to pay that cost —
  when a non-lock-stepped worker actually exists — not speculatively now.
- The `HandshakeResponse { agreed }` shape is unchanged: `agreed` always equals
  `offered` on success (because success means equality), so the negotiation
  envelope and its round-trip tests are untouched.
- Touch radius is small and contained to the wire boundary: `voom-core`
  constants, the `ProtocolError` variant, `negotiate`/`enforce_version`, the
  `chaos_worker` fake's handshake, and the conformance suite's
  below-window probe. No store, event, or migration surface is involved —
  worker version skew is the wire axis, distinct from the durable-payload axis of
  ADR 0013.

## Considered & rejected

- **Honour the range — make `enforce_version` accept `[MIN, MAX]` like
  `negotiate`.** The minimal-diff fix: keep the error shape, widen the middleware.
  Rejected because it preserves a window the bundled-worker deployment never
  exercises and `transcode_video.rs` explicitly denies — codifying a phantom
  skew-tolerance contract that no integration test could meaningfully cover (there
  is no second worker version to offer). It also leaves two independent copies of
  the comparison free to drift again. Exact-match is the honest contract for
  lock-stepped releases and is strictly simpler (AGENTS.md Rule 3).
- **Keep two independent exact-match checks** (fix `negotiate` to exact-match but
  leave `enforce_version` as its own copy). Rejected: it fixes today's divergence
  but not the *mechanism* — the acceptance criterion is that the two paths "agree
  on one documented contract," which a shared definition guarantees structurally
  and two copies guarantee only by vigilance.
- **Keep `supported_min`/`supported_max` fields but set both to
  `PROTOCOL_VERSION`** (drop only the constants, not the error fields). Rejected:
  the redundant pair still advertises a range vocabulary on the wire, inviting the
  same misreading the issue flags; collapsing to a single `expected` says exactly
  what the contract is.
- **Delete the handshake entirely and rely only on the per-request header.**
  Rejected: the handshake is the fail-fast compatibility gate — it rejects a
  mismatched worker once, at startup, instead of on every operation, and is the
  documented Phase-1 negotiation seam. Exact-match narrows what it accepts; it
  does not remove the value of checking early.
