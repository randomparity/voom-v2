---
title: ffmpeg preflight probe deadlocked because the child's piped output was never drained
date: 2026-07-29
tags: [deadlock, subprocess, pipe-capacity, api-misuse, environment-quirk]
components:
  [
    crates/voom-ffmpeg-worker/src/preflight.rs,
    crates/voom-cli/tests/multi_phase_flow.rs,
  ]
---

## Problem

Every `voom-ffmpeg-worker` startup failed preflight after burning the full 15-second
probe deadline:

```
Error: Dependency { detail: "ffmpeg preflight failed: ffmpeg -hide_banner -encoders failed to start: dependency probe exceeded 15 seconds" }
```

The worker therefore exited before printing its bind line, which failed the CLI
integration test with a misleading message about the binary rather than about ffmpeg:

```
called `Result::unwrap()` on an `Err` value: Custom { kind: Other, error:
  "<repo>/target/debug/voom-ffmpeg-worker exited before bind line" }
test multi_phase_execute_then_report_by_job_id ... FAILED
test result: FAILED. 0 passed; 1 failed; finished in 15.63s
```

Misleading signals that cost time:

- `ffmpeg -hide_banner -encoders` run **directly** in a shell completed in 0.06s with
  exit 0. ffmpeg itself was healthy.
- The error text says `failed to start`, which points at a missing or non-executable
  binary. `/usr/bin/ffmpeg` existed and was executable. The string comes from
  `command_text`'s `io::Error` arm (`preflight.rs`), which cannot distinguish a spawn
  failure from a timeout while waiting.
- The failure looked like host contention (it first appeared while `prek` was
  compiling in parallel), but it reproduced identically with an idle machine and on
  the parent commit `0a0b6dec`, so it was neither flaky nor newly introduced.

## Root cause

`wait_child_output_io` set `Stdio::piped()` on stdout and stderr, then waited by
polling `child.try_wait()` in a `thread::sleep(10ms)` loop **without ever reading the
pipes**.

A child that writes more than the OS pipe capacity blocks in `write()` until a reader
drains it. A blocked child never exits, so `try_wait()` never returns `Some`, so the
loop can only ever end at the deadline. The deadlock is unconditional once output
exceeds capacity — nothing about it is timing-dependent.

Why it bit this host and not CI:

| quantity                                  | value  |
| ----------------------------------------- | ------ |
| `ffmpeg -hide_banner -encoders` on stdout | 14085 bytes |
| pipe capacity on the affected host        | 8192 bytes  |
| default Linux pipe capacity               | 65536 bytes |

14085 bytes fits comfortably in a default 64 KiB pipe, which is why the code worked
everywhere until now and why "pipe deadlock" looked ruled out at first. This host's
pipes were 8 KiB because the kernel shrinks new pipe buffers once a user crosses
`fs/pipe-user-pages-soft` (16384 pages here).

Measure actual capacity — do not assume 64 KiB:

```bash
python3 -c "import fcntl, os; r, w = os.pipe(); print(fcntl.fcntl(w, 1032))"  # F_GETPIPE_SZ
cat /proc/sys/fs/pipe-user-pages-soft /proc/sys/fs/pipe-max-size
```

Reproduce the deadlock independently of ffmpeg, by mimicking the poll loop:

```python
import subprocess, time
p = subprocess.Popen(["ffmpeg","-hide_banner","-encoders"],
                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
start = time.time()
while p.poll() is None:                      # never reads the pipes
    if time.time() - start > 20:
        print("TIMED OUT -> pipe deadlock"); p.kill(); break
    time.sleep(0.01)
```

The underlying API mistake is general: `std::process::Child::wait_with_output()`
drains concurrently with waiting precisely because you must. Hand-rolling a
*timeout-capable* wait around `try_wait()` silently dropped that invariant. Any
"wait with a deadline" helper over piped stdio inherits this trap.

## Solution

Fixed in `7accb54e`. `wait_child_output_io` (`crates/voom-ffmpeg-worker/src/preflight.rs:757`)
now moves each pipe onto its own drain thread for the whole wait, and builds `Output`
from the joined buffers instead of calling `wait_with_output()` after the fact.

One non-obvious detail, and the reason for the second regression test: **on the
timeout path the drain threads are detached, not joined.** Killing the direct child
does not close a pipe that a surviving grandchild still holds. The first version of
the fix joined them, and a `#!/bin/sh` + `sleep 60` stub turned the 200ms deadline
into a 60-second hang — converting a bounded timeout into exactly the startup hang the
deadline exists to prevent.

Both behaviors are pinned by unit tests in
`crates/voom-ffmpeg-worker/src/preflight_test.rs`:

- `command_output_drains_a_child_that_outwrites_the_pipe_capacity` — a stub writing
  256 KiB (overshooting any plausible capacity, since it is host-dependent) must
  return `Ok` with all bytes. Fails as a 15s timeout without the drain.
- `wait_child_output_times_out_promptly_when_a_grandchild_holds_the_pipe` — asserts
  **elapsed < 5s**, not merely that the error is `TimedOut`. Returning `TimedOut`
  eventually is not the same as returning promptly, and only the elapsed assertion
  catches the join regression.

Verification:

```
$ cargo test -p voom-ffmpeg-worker --lib preflight
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 80 filtered out; finished in 0.21s
   (was 60.00s while the fix still joined the drains; 15.01s and 1 FAILED before the fix)

$ cargo test -p voom-cli --test multi_phase_flow
test result: ok. 1 passed; 0 failed; finished in 2.03s
   (was FAILED in 15.63s)
```

## Prevention

- The two tests above are the regression guard. Both are cheap and hardware-free.
- **Convention:** do not hand-roll a wait loop over piped stdio. Prefer
  `wait_with_output()`; when a deadline is required, drain on threads for the entire
  wait and detach them on the failure path. A `try_wait()` loop plus
  `Stdio::piped()` and no reader is always a latent deadlock.
- No clippy lint covers this. If it recurs, the mechanical signature is greppable —
  a `try_wait()` polling loop in a function that configures `Stdio::piped()` — and
  would suit an `ast-grep` rule alongside the existing `scripts/check-*.sh` guards.
- `command_text` reports a timeout with the words `failed to start`, which sent the
  investigation at the binary rather than the wait. Worth splitting those arms if
  this area is touched again.
- Do not assume 64 KiB pipes when reasoning about subprocess output sizes. Measure
  with `F_GETPIPE_SZ`; `fs/pipe-user-pages-soft` pressure shrinks new pipes, so a
  developer host can deadlock on output that CI handles fine.
