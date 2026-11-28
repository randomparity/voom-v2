# Crash-safe shared ffprobe fixture lock (#483)

## Scope and success

Repair only `TargetDebugLock` and sibling unit tests. C1: a dead holder cannot
poison later runs. C2: concurrent revised test binaries serialize fixture mutation.
C3: contention remains bounded to two minutes and timeout explains safe cleanup.
C4: regression tests exercise actual processes and the timeout text.
Authority: #483 and WORK:SCOPE q483-0bf14dc7, including operator decisions
“DECISION: Approve one extra review for #483” and
“DECISION: Correct and review before merge”. Outcomes do not prescribe PID storage.
Hidden ffprobe restoration after death remains owned by #501.
Public helper signatures, guard cleanup order and fixture ownership remain intact.

## Design

Use the persistent file `.voom-ffprobe-sibling.lock-owner` directly in the profile
returned by `target_debug_dir()`. That parent also owns executed test/worker binaries
and is already a trusted directory. Never open or write inside the legacy
`.voom-ffprobe-sibling.lock` directory: it may be abandoned and peer-writable,
with an attacker-controlled `owner` symlink. Its contents remain untouched.
Open the new file read/write without truncation, creating new Unix files with
mode0600 so another account cannot acquire the lock through read access.
Use the standard library's exclusive `File::try_lock` (stable since Rust1.89;
repository pins1.95). The guard owns the descriptor. Never unlink the file:
waiters must resolve to one inode. `FfprobeSiblingGuard` still restores/removes
fixtures before its lock field drops. Kernel ownership ends after the final
owning descriptor closes, including on death. A pre-exec inherited descriptor
can delay release until close; spawned helpers close descriptors on exec.
A private `acquire_at(&Path, Duration)` seam accepts the profile directory and
selects the fixed sibling internally. Production and isolated regressions share
that path-selection code. Production `acquire()` supplies the running profile
and two-minute cap. Retry only `TryLockError::WouldBlock`, polling every20ms.
Other errors preserve kind and identify the operation with an escaped path.
Timeout uses escaped OsStr Debug for unambiguous identification, says the state
may be stale, and gives a fixed relative command from the affected profile:
`rm -f -- .voom-ffprobe-sibling.lock-owner`. Stop all test processes first.
No path bytes become shell operands. Unsupported locking errors fail loud.

Existing abandoned legacy directories do not affect revised acquisition.
A regression leaves a dangling owner symlink in a peer-writable legacy directory
and proves its external target stays absent while acquisition succeeds.
PID/boot liveness and reclaim elections are unnecessary: the kernel is the oracle.
Concurrent old or intermediate in-directory-lock binaries cannot interoperate;
stop all processes before switching generations. Rollback also requires clearing
legacy state after quiescence. New state consists solely of the persistent file.
No dependency or architectural policy changes warrant a new ADR.

## Failure model

Deployments are local Linux/macOS test runs on ordinary lock-capable filesystems,
including custom target roots, with trusted profile parents. Code execution from
those parents already depends on their integrity. A writable legacy lock directory
must never become an authority for new filesystem writes or ownership.
Death after acquisition releases ownership after the last descriptor closes.
Competing opens and normal Drop must preserve one inode and exclude simultaneous
owners. Open/locking errors propagate, and timeout never deletes live state.
Tests use temporary paths, bounded handshakes and explicit ignored child entries;
positive acquisitions poll for final-descriptor release, including transient
pre-exec inheritance from concurrent test spawns. Negative exclusion uses zero timeout;
only holder/a/b roles are accepted before marker paths are constructed.
Raw-byte filesystem tests run on Linux; macOS receives legal UTF-8 shell-sensitive
cleanup coverage because APFS rejects invalid UTF-8 names.
Unsupported/network semantics, manual live unlink, and concurrent legacy binaries
are outside this guarantee. Hidden sibling repair remains excluded (#501).

## Validation

`cargo test -p voom-test-support worker::tests` covers legacy orphan and dangerous
legacy symlink compatibility, death, live contention, concurrent waiters, stable
inode identity, file permissions, child-entry isolation, I/O diagnostics and cleanup.
Repeat the process test and run serially. `just ci`, mandatory hooks and remote CI
cover callers, repository conventions and supported platforms. Report ignored helper
entries explicitly; parent tests select and execute them in real subprocesses.
