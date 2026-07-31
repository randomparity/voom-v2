# NVIDIA and VideoToolbox decoder diagnostics are captured and discarded

## Status

Open, review-by: 2026-10-31

## Concern

`NvidiaPreflight.decoder_diagnostics` (`crates/voom-ffmpeg-worker/src/preflight.rs`,
struct at line 38, populated at line 344) and
`VideoToolboxPreflight.decoder_diagnostics` (line 57, populated at line 389) each
record why a per-codec decode probe failed, and neither is ever read. `rg
decoder_diagnostics crates/ --glob '!*_test.rs'` returns only the struct
definitions and the assignments that fill them — there is no consumer for either
backend.

The consequence is the one ADR 0052 §2 names for VAAPI and that applies equally to
the other two probe-proven backends: the per-start probe is the only detector of
driver-build capability drift, so when a codec stops proving, the worker still
becomes ready and the codec simply disappears from `decoders`. Scheduling then
reports the source as incompatible, or the candidate set comes back empty, with
nothing anywhere stating which codec failed its probe or why. The artifact that
explains the failure is computed and thrown away.

## Why deferred

The fields predate this charter: they arrived with the NVIDIA slice (#410) and the
VideoToolbox slice (#411), and this branch neither added them nor made them worse.

This charter's completion criteria require **software and NVIDIA behavior
byte-for-byte unchanged**. Emitting NVIDIA startup diagnostics changes that
backend's observable startup output, which is outside the permitted surface. It is
also unverifiable here: the development host is AMD, `nvidia-smi` is absent, and
this slice's NVIDIA re-verification is already an open request against a machine
with the hardware. Changing an NVIDIA startup path that cannot be executed once
before merge trades a documented diagnostic gap for an unproven regression risk on
a backend nobody can exercise in this run.

VAAPI's own instance was **not** deferred. `probe_vaapi_decoders`'s doc comment
promised the reason is "retained rather than dropped" while discarding it — a false
promise this branch introduced — so that contribution was fixed here
(`report_unproven_vaapi_decoders` in `crates/voom-ffmpeg-worker/src/main.rs`, plus
the matching runbook row). This record owns the residual: the two backends whose
identical gap this branch inherited.

## Non-regression boundary

This branch must not make the sibling gap worse, and does not:

- No NVIDIA or VideoToolbox preflight, probe, or descriptor code path is modified
  by this branch; both `decoder_diagnostics` fields are exactly as #410 and #411
  left them.
- The VAAPI fix is additive and backend-scoped —
  `report_unproven_vaapi_decoders` returns immediately unless `preflight.vaapi` is
  `Some`, so a worker bound to NVIDIA or VideoToolbox, or to no accelerator at all,
  emits nothing new.
- Nothing was added to the durable `deny_unknown_fields` accelerator descriptors, so
  the payload contract is untouched for all three backends and no migration or
  rollback consequence follows from the fix.

## What would resolve it

Emit the captured reasons for all three backends from one shared helper, on worker
stderr before the `BOUND` line, and add the matching row to each backend's
startup-diagnostics table in `docs/runbooks/operator-real-media-execution.md`
(VAAPI's row already exists). Keep the reasons out of the advertised descriptors —
they are operator diagnostics, not persisted capability.

Done when a worker whose decode probe fails for a codec on **each** backend prints
one line naming the codec, the device, and the reason; when
`rg decoder_diagnostics` shows a non-test consumer for all three; and when the
NVIDIA path has been executed once on a host with the hardware. If the reasons are
instead judged not worth surfacing, the resolution is the opposite and equally
complete: delete both fields and the doc comments promising retention, so no field
claims a guarantee nothing keeps.

## Provenance

target: crates/voom-ffmpeg-worker/src/preflight.rs
target: crates/voom-ffmpeg-worker/src/main.rs
Raised by challenge review iteration 5 of the issue #409 VAAPI slice
(run `challenge-409-r5-7f3a9c21`), 2026-07-30, as the third instance of a pattern
whose VAAPI instance was fixed in the same change.
