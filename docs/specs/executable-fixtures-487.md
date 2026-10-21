# Executable fixtures without inherited writers (#487)

## Problem
Parallel tests write temporary executable scripts while sibling tests fork.
A child may inherit a writable descriptor until exec/exit, causing Linux ETXTBSY
when the owner executes its fixture after closing its own handle. The #709 hook
reproduced #487's exact VideoToolbox failure. A controlled fork harness reproduced
errno 26 while an inheriting child lived, then success after it exited.

## Scope
Implement #487's workspace fixture criterion in shared voom-test-support.
Add only dev-dependency edges for the three media workers missing test-support;
node-agent, control-plane and CLI already depend on it for tests. Preserve fixture
bytes, paths and intended final permission modes, including node-agent's 0700.
Migrate script writers and the copied fake-ffprobe fixture; remove obsolete local
0755 helpers. Preserve negative, non-executable fixtures as ordinary file writes.
The frozen charter is the latest WORK:SCOPE for q487-a1e2964b on #487.
No production spawn/engine/DSL changes, media writes, production starts or cleanup.
No deferrals. ADR 0105 governs the fixture writer's process boundary.

## Design
Expose `voom_test_support::executable::write_executable(path: &Path,
contents: impl AsRef<[u8]>) -> io::Result<()>`.
On Unix run `/bin/sh -c 'cat > "$1" && chmod 755 "$1"' fixture-writer PATH`;
send bytes over piped stdin, inherit stderr, close stdin, and wait/reap before
returning. Inheriting stderr prevents diagnostics from blocking input transmission. Pass the
path as an argument and bytes as data. The test process never opens the destination
for writing, so concurrent sibling forks cannot inherit its writable descriptor.
The writer shell waits for its writing child before exiting. Propagate spawn,
input, wait and nonzero child failures. No ETXTBSY retries are introduced.
On non-Unix retain ordinary file-writing behavior; executable shell fixtures are
Unix-only. The caller owns the destination and temporary directory. The existing
copy helper reads its source and delegates fixture writing before returning.

## Success
The inventoried workspace write-then-exec fixtures use the shared child writer.
The original full parallel FFmpeg crate run and repeated capacity test pass.
Focused tests prove exact bytes (including large input and shell metacharacters),
immediate execution, preserved executable mode, and surfaced write failures.
Required hooks and final `just ci` pass on the complete branch.

## Failure model
- Actors and deployments: trusted Rust unit/integration tests on Unix CI and local
  supported hosts; non-Unix compile behavior preserved, shell fixtures Unix-only.
- Invariants and assets: repeatable executable fixtures; exact content; original
  destinations/modes; child completion and genuine error reporting.
- Accepted failure classes: missing `/bin/sh`, `cat` or `chmod`, inaccessible paths,
  full disks and non-executable filesystems fail tests visibly; external edits or
  independently opened writers are outside this temporary-fixture ownership model.
- Covered elsewhere: worker relinking is prebuilt by just test/test-hook (#586);
  production subprocess behavior is unchanged; caller TempDir owns fixture cleanup.

## Threat model
- Boundary inventory: add a test-only shell writer receiving destination argv and
  content stdin; retain caller-owned temporary paths. No production boundary widens.
- Actor model: repository test code and CI PATH are trusted; fixture content and
  filename strings are data, including metacharacters, not shell source.
- Control per boundary: fixed shell program/source, quoted positional path, stdin
  bytes, completed/reaped child and surfaced failures; preserve final permission
  modes. Test path metacharacters and exact bytes. No shell interpolation of data.
- Out of scope: malicious test code, compromised CI tools or independently opened
  destinations; these have the test process's authority already.

## Validation
- Focused tests: shared helper executes exact content with quoted paths, binary/
  large content preserved, invalid destination surfaces error (including large
  diagnostics plus large input, checked by a bounded subprocess regression). Existing full FFmpeg
  parallel failure is the red baseline; the controlled inheritance harness confirms
  cause. Exercise parallel helper creation and execution without retries.
- Migration: inspect every executable permission writer and copied test binary in
  workspace; run affected crate tests through workspace hooks/full CI.
- Prose/ADR: task-test-not-applicable; human decision text has no executable reader;
  use repository ADR/index structure guard, not wording snapshots.
