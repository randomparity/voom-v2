# The VAAPI capacity probe does not prove sessions overlap

## Status

Open, review-by: 2026-10-31

## Concern

`prove_vaapi_capacity` (`crates/voom-ffmpeg-worker/src/preflight.rs`) spawns
`max_sessions` copies of `vaapi_encode_probe_command` and then waits for each. It
spawns every child before reaping any, so overlap is likely — but nothing
establishes it. That command is
`-f lavfi -i testsrc=size=256x256:rate=1 -frames:v 1` with no `-re` and no `-t`:
each process encodes exactly one 256x256 frame and exits in tens of milliseconds.
On a slow spawn path the first can therefore exit before the last starts, and
`max_sessions` sequential encodes prove only that the device can encode at all.

The VideoToolbox sibling proves its declared groups with explicit first-frame and
all-live evidence. VAAPI's probe is weaker than that while the operator
documentation described it in the same terms.

## Why deferred

Making it a real proof means holding every session open simultaneously and
observing all of them live at one instant — a longer per-probe encode plus
liveness checks against `capacity_clock`. That is a timing-sensitive startup path
on a backend where VAAPI exposes no session enumeration at all, so a failure
cannot be attributed to a cause (already recorded in the operator runbook's
diagnostics table). Finding F14 in the issue-409 plan records that this repo's
clock-expiry probe tests are the ones that flake under parallel load; adding more
wall-clock-sensitive startup assertions is exactly the change that should be made
deliberately, with its own test strategy, rather than folded into this branch.

## Non-regression boundary

This change must not make an over-declared capacity easier to configure or harder
to notice, and it does not:

- `--vaapi-max-sessions` is bounded to `1..=16` by clap, on the CLI and via
  `VOOM_VAAPI_MAX_SESSIONS`, and defaults to 1 — the value at which the concern
  is vacuous.
- The probe still runs, still fails startup when any session's encode fails, and
  still kills and reaps its siblings on the first failure.
- Capacity is enforced at lease acquisition against the declared value, so an
  over-declaration degrades throughput rather than corrupting output.
- The operator documentation no longer claims the probe proves concurrency; it
  states what the probe establishes.

The residual, and the whole of it: a device that cannot sustain the declared
session count may still pass startup, and the failure appears later under load.

## What would resolve it

Hold every probe session open at once — a bounded encode long enough to overlap —
and assert all are live at a single observation before any is reaped, mirroring
the first-frame/all-live evidence the VideoToolbox capacity proof already
produces. Done when a test declaring a capacity the fake device cannot sustain
fails startup, and one declaring a sustainable capacity passes, with neither
depending on process scheduling luck.

## Provenance

target: crates/voom-ffmpeg-worker/src/preflight.rs
target: docs/runbooks/operator-real-media-execution.md

Raised by `/review-loop --base main` on the issue-409 VAAPI branch, iteration 3,
2026-07-30.
