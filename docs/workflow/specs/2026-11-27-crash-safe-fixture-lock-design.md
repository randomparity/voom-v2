# Crash-safe shared ffprobe fixture lock (#483)

## Scope and success

Repair only `TargetDebugLock` and sibling unit tests. C1: a dead holder cannot
poison later runs. C2: concurrent revised test binaries serialize fixture mutation.
C3: contention remains bounded to two minutes and timeout explains safe cleanup.
C4: regression tests exercise actual processes and the timeout text.
Authority: #483 and WORK:SCOPE q483-0bf14dc7. The operator approved outcomes,
not PID storage. Hidden ffprobe restoration after death remains owned by #501.
Public helper signatures, guard cleanup order and fixture ownership remain intact.

## Design

Keep the existing `.voom-ffprobe-sibling.lock` directory and create a persistent
`owner` file inside it. Open read/write without truncation, then use the standard
library's exclusive `File::try_lock` (stable since Rust 1.89; repository pins 1.95).
The guard owns the file handle; closing the final owning descriptor releases the lock, including on process death.
Spawned helpers close descriptors on exec; a pre-exec inherited descriptor can delay
release until it closes.
Never remove the file or directory: unlink/recreate would let waiters lock different
inodes. `FfprobeSiblingGuard` still restores/removes fixtures before its lock field drops.
A private `acquire_at(&Path, Duration)` seam permits isolated deterministic tests;
production `acquire(&str)` supplies the current profile path and two-minute cap.
Poll only `TryLockError::WouldBlock`, every 20 ms. Other errors preserve their
kind and identify the failed operation and lock path. Unsupported locking fails loud.

Existing empty abandoned directories work immediately. No PID, boot identity,
mtime, or cleanup election is needed because the kernel is the ownership oracle.
Concurrent binaries must use this revised helper; older create_dir binaries cannot
interoperate and must be stopped before upgrading/rebuilding. No cross-version
handoff is promised. The directory/file are harmless persistent fixture artifacts.
Timeout says the state may be stale and provides a fixed relative cleanup command,
explicitly conditioned on stopping all test processes first and running it from
inside the affected lock directory. No path bytes are converted into shell operands. Manual deletion while
processes run violates the stable-inode invariant and is never advised.

Alternatives: PID/boot/start metadata needs platform-specific liveness and a separate
serialization protocol for reclaim (a waiter can otherwise unlink a successor).
mtime expiry cannot distinguish a slow live holder. Retaining create_dir alone
cannot meet C1. Kernel file locking is the smaller implementation and needs no dependency.
No architectural policy changes warrant a new ADR for this fixture-local mechanism.

## Failure model

Named deployments are local Linux/macOS test runs on ordinary lock-capable filesystems,
including custom target roots and CI. Death at any acquisition or held-lock point
must release kernel ownership after the last inherited/duplicated descriptor closes; an empty legacy directory must remain usable.
Competing opens, contenders already waiting on death, and normal Drop must retain
one inode and exclude simultaneous owners. Open/create/locking failures propagate.
Contention times out without deleting an active owner's state. Tests use temporary
paths and bounded handshake polling, never the real shared ffprobe slot or paused time.
Unsupported/network filesystem semantics, manual external deletion during live runs,
and concurrent legacy binaries are outside the supported deployment guarantee;
unsupported OS lock errors are surfaced. Hidden sibling repair is excluded (#501).

## Validation

`cargo test -p voom-test-support worker::tests` covers abandoned legacy directories,
process death, live contention, concurrent waiters, stable lock identity after Drop,
path/error diagnostics and lossless cleanup for non-UTF-8 target roots. Repeated process tests and serial/parallel
execution challenge timing. `just ci` and mandatory hooks cover callers and repository
conventions; remote CI covers supported platforms. No performance benchmark is needed.
