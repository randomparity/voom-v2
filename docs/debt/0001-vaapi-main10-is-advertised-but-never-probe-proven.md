# VAAPI Main10 is advertised but never probe-proven

## Status

Open, review-by: 2026-10-31

## Concern

ADR 0052 §2 makes probe-proven capability the rule for this backend: the worker
never infers what a device can do from FFmpeg's encoder list, because advertised
capability tracks the *loaded VA driver build*, which is invisible from the
render node. The startup probe honours that for 8-bit only — it encodes `nv12`
and nothing else (`crates/voom-ffmpeg-worker/src/preflight.rs`, the
`hevc_vaapi` encode probe). A worker that becomes ready therefore advertises
`hevc_vaapi` unqualified, and the scheduler will hand it a `p010`/`main10`
profile on a host where 10-bit encode was never executed once.

Half of what issue #409 promises is 10-bit, so this is not a corner: the
capability the descriptor asserts is broader than the capability it proved.

## Why deferred

Recording the result needs somewhere to put it, and there is nowhere.
`VaapiVideoAcceleratorDescriptor.encoders` is a `Vec<String>` of bare encoder
names — unlike `VideoToolboxDecodeCapability`, which carries a `pixel_formats`
list per codec. Proving Main10 at startup therefore requires either a new
descriptor field or a per-encoder capability struct, on a type carrying
`#[serde(deny_unknown_fields)]` whose JSON is persisted in
`worker_operation_capabilities` and read back by older builds. That is a
worker-protocol change plus a migration for stored rows — a material expansion
of this charter, not a fix inside it.

The alternative available today is worse: making the 10-bit probe a hard startup
requirement would refuse an otherwise usable 8-bit device, turning a partial
capability into no capability.

## Non-regression boundary

This change must not make a Main10 failure *quieter* or later than it already
is, and it does not:

- The startup probe's 8-bit guarantee is unchanged.
- A 10-bit source paired with an 8-bit VAAPI surface under hardware decode now
  fails at config validation with a typed, actionable error naming both formats
  (`validate_vaapi_bit_depth`), where it previously reached FFmpeg and surfaced
  as `WORKER_CRASH` wrapping `No usable encoding profile found`.
- `scripts/accept-vaapi-video-acceleration.sh` verifies Main10 end-to-end on
  real hardware, so the gap is an *advertisement* gap, not an unverified path.
- The operator runbook states the boundary explicitly under "Main10 is proven by
  acceptance, not by the startup probe".

The residual, and the whole of it: a `p010` profile scheduled to a device whose
driver build cannot encode Main10 fails when FFmpeg runs, rather than being
excluded at selection.

## What would resolve it

Extend the VAAPI descriptor to record proven encode formats per encoder — the
shape `VideoToolboxDecodeCapability` already uses for decoders — probe `p010`
at startup as a non-fatal additional probe, and record the outcome. Scheduler
selection then excludes a device that did not prove the surface the profile
names, and the worker's own binding validation rejects the assignment as it
already does for decoders (`validate_vaapi_decoder_probed`).

Done when: a host whose driver cannot encode Main10 refuses a `p010` profile at
selection time with a message naming the PCI address, proven by a test that
advertises an 8-bit-only descriptor and asserts the `p010` profile is not
scheduled to it.

## Provenance

target: crates/voom-ffmpeg-worker/src/preflight.rs
target: crates/voom-worker-protocol/src/video_acceleration.rs
target: docs/adr/0052-vaapi-device-identity-and-probe-proven-capability.md

Raised by `/review-loop --base main` on the issue-409 VAAPI branch, run
`challenge-409-r1`, 2026-07-30. Recorded as finding F9 in
`docs/superpowers/specs/2026-07-29-issue-409-vaapi-video-acceleration-plan.md`.
