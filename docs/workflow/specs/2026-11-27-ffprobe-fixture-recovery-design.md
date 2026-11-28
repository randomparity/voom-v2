# Interrupted ffprobe fixture recovery (#501)

## Problem and authority
Issue #501 identifies hidden ffprobe siblings stranded when a test process dies.
The approved scope comment q501-3068ae83 covers fixture recovery; #483 already
owns shared lock recovery. The existing worker helper remains the owner.

## Scope and constraints
Change worker.rs and its sibling worker_test.rs only, plus these design artifacts.
Keep public helper signatures and the trusted profile-parent lock path unchanged.
Use the current Rust toolchain and std filesystem APIs; add no dependencies.
Linux and macOS CI are the named supported deployments. Preserve ordinary
cleanup on other hosts; refuse occupied-sibling recovery without proven identity.
No ADR or migration is assigned. No caller migration is required.

## Design
Use a deterministic `.voom-ffprobe-sibling.fixture` directory in the trusted
profile parent, holding `original` and `replacement`. Acquire the existing lock
before recovery or mutation. Do not inspect legacy writable lock directories.
For installation, create the recovery directory and rename an existing sibling
into `original` before recording a replacement witness. Stage the executable
with ADR 0105's child writer in a unique tempfile directory under the profile
parent. Wait for successful writer exit, then rename the completed inode into
`replacement` and publish it by hard-linking to the sibling path on Unix.
Elsewhere, record an empty replacement marker and rename the staged file into
the vacant sibling slot; normal publication needs no hard-link support. An orphaned
writer only addresses its unique directory, which retries do not reuse.
For hide, rename a recognized test helper into `original` in the same directory.
Recovery checks the journal entries before making changes. If sibling identity
matches replacement, unlink it. If occupied with recorded entries but not the
replacement identity, refuse. An empty journal implies no outstanding mutation
and can be removed without touching the sibling. Remove `replacement` before
rename-restoring `original` to an absent sibling. Finally remove the empty
journal. Rename preserves original symlinks and avoids new hard-link privilege
requirements for restoring the original. The replacement witness provides Unix
device/inode identity. An interrupted cleanup cannot leave a restored original
with a replacement witness because that witness is retired first.
An occupied sibling matching neither recorded inode is a conflict: retain both
sibling and recovery data and return an actionable error. On non-Unix hosts,
normal live guards retain their existing cleanup ownership; abandoned occupied
sibling recovery refuses to guess. Empty/incomplete staging is safely discarded.
Treat unexpected journal entries and symlink journal directories as conflicts.
Legacy `ffprobe.*.hidden.*` backups lack ownership evidence: refuse and identify
manual recovery rather than guessing which original to restore.
Drop calls the same recovery routine while holding the lock; cleanup errors are
reported to stderr and the journal remains for retry. Failed setup attempts
recovery before returning the original error kind; if recovery also fails, include
both failures and the retained journal path in that error. Labels remain accepted but do not
construct recovery paths.

## Success
In benign temporary fixtures, normal guards restore the original bytes, killed
processes release the lock and subsequent guards on Unix recover original state, and
retry after partial setup/restoration is idempotent. Recovery preserves a foreign
sibling and reports the journal path. Existing callers keep serialization.

## Failure model
- Actors and deployments: cooperative local/CI test processes on Linux/macOS;
  other hosts retain normal cleanup with conservative interrupted recovery.
- Invariants and assets at stake: original sibling bytes; foreign siblings;
  one fixture owner under the existing lock; exact caller signatures.
- Accepted failure classes: power loss/filesystem corruption (no fsync durability
  promised for disposable build outputs); same-account hostile concurrent writes
  (trusted profile parent); legacy ambiguous backups (manual recovery required);
  unavailable inode identity outside Unix (occupied recovery refuses safely);
  orphan writer staging directories (one disposable build-output directory per
  interrupted stage, never reused or removed while its writer may be alive).
- Covered elsewhere: kernel lock lifetime and concurrency — #483;
  production worker routing — existing control-plane callers.

## Threat model
- Boundary inventory: existing source-file bytes and profile-parent entries are
  consumed by the helper; adds private journal entries, no widened public entry.
- Actor model: cooperative local operator and CI job; profile parent is trusted.
- Control per boundary: lock serializes helpers; reject symlink journal roots and
  unexpected entries; labels do not form paths; compare symlink metadata for
  identity; conflicts retain evidence and escaped-path diagnostics.
- Out of scope: hostile same-account concurrent mutations and power loss, because
  these are disposable trusted build outputs, not production durable state.
