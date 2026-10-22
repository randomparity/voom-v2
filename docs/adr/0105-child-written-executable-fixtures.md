# 0105: Write executable fixtures in child processes

## Status
Accepted

## Context
Parallel tests can fork while another test writes a temporary executable.
An inherited writable descriptor makes exec return ETXTBSY until that child
execs or exits. The #709 pre-commit hook reproduced the #487 fixture failure;
a controlled fork harness reproduced errno 26 after the parent closed its own
writer, and success after the inheriting child exited.

## Decision
Shared voom-test-support writes executable fixtures in a child process and waits
for completion. Test processes pass bytes through stdin and never open the
executable destination for writing. Migrate the workspace's matching fixture
writers, including copied fake executables. Production spawn behavior is unchanged.

## Consequences
Unix fixtures require the same shell environment their scripts already use,
plus cat and chmod. Each fixture adds a short-lived writer process. Errors remain
visible; the caller still owns temporary paths and cleanup. Non-Unix file writing
retains its existing behavior. Internal test-only dev-dependencies use the existing
support crate; no production dependency or toolchain floor changes.

## Considered & rejected
- Bounded spawn retries — judgment: scattering retries across synchronous and
  asynchronous test consumers treats the inherited writer rather than removing it.
- Rename after writing — verified: Linux exec checks the inode's writer state,
  independent of the pathname; the controlled fork harness holds that same inode.
- Global serialization — judgment: every concurrent spawn would need to share the
  lock, expanding this fixture repair into process-wide synchronization.
- Keep existing writers — judgment: leaves the reproduced hook failure unresolved.
