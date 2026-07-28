---
status: proposed
date: 2026-07-28
issue: 368
---

# Expected-aware artifact promotion copy

## Goal

Reduce redundant full-file reads during host-owned artifact promotion only when
measurement shows a material improvement, without weakening exact-byte
validation, source-mutation detection, add-only installation, durability, or
crash recovery.

## Current behavior

The general staged-artifact commit and the recoverable audio-sidecar commit both
have immutable expected size and BLAKE3 facts before promotion. Their copy path
does not use those facts:

1. observe and hash the staged file;
2. observe and hash it again inside `copy_regular_file_checked`;
3. read it a third time while copying;
4. observe and hash the temporary copy;
5. in the audio path, observe and hash the temporary copy again immediately
   before installation; and
6. observe and hash the installed target.

The exact count differs around durable hooks, but a fresh audio-sidecar
promotion performs six full-file reads plus one full-file write. Those reads
preserve real safety properties, so an optimization must identify which checks
can be combined rather than simply deleting observations.

## Decision gate

Prototype an expected-aware copy and retain it only if all of these gates pass:

- the median wall time improves by at least 20% at both 64 MiB and 256 MiB;
- no measured size has a regression greater than 5%;
- seven measured samples follow two warmups for each implementation and size;
- baseline and candidate use the same deterministic non-sparse source bytes,
  filesystem, build profile, and machine;
- each sample includes copy, file fsync, no-replace install, directory fsyncs,
  and final target validation;
- benchmark results, environment, and exact commands are recorded in this
  document; and
- every safety and recovery test passes.

The benchmark represents recently generated media, for which source bytes are
normally warm in the page cache. It does not claim raw-device throughput.
Twenty percent is large enough to exceed normal local timing noise and matter
for ordered multi-output extraction. If the candidate misses the gate, remove
the prototype and close the issue with the measurements supporting the current
implementation.

## Candidate

Add one internal expected-aware copy primitive used only when the caller already
owns trusted expected facts:

```text
copy_regular_file_with_expected(source, destination, expected)
    -> source_facts
```

The primitive:

1. opens the source with the existing no-follow regular-file check;
2. snapshots source metadata;
3. creates the canonical destination with `create_new`;
4. reads bounded chunks, updates BLAKE3 and byte count, and writes each chunk
   completely;
5. flushes the destination;
6. re-reads source and destination metadata;
7. rejects source metadata drift, destination length drift, arithmetic
   overflow, or a copied size/hash that differs from the expected facts;
8. fsyncs the destination file; and
9. returns source facts derived from the copied stream and final source
   metadata.

Any error after destination creation removes only that owned create-new leaf.
Cleanup failure remains visible alongside the primary failure.

Hashing the bytes read for the copy combines staged-file validation with the
copy read. The primitive deliberately does not label the stream hash as an
independently observed destination hash. The surrounding promotion paths
continue to hash:

- the temporary sibling immediately before add-only installation; and
- the installed target after installation.

These reads detect write corruption and mutations in the interval between copy
and install. The generic artifact path retains its pre-hook staged observation
and its post-install target observation; its expected-aware copy occurs after
the hook, so hook-time source drift is rejected.

## Call-site behavior

### Fresh audio-sidecar promotion

`promote_staged_add_only` and `promote_staged_add_only_with_temp` obtain staged
facts from the expected-aware copy rather than pre-observing and then invoking
the generic checked copy. `promote_staged_add_only_from_temp` remains unchanged:
it re-observes the temp, installs with no replacement, and re-observes the
target.

The fresh path therefore falls from six full-file reads to three:

1. copy and hash staged bytes;
2. hash the temp before install; and
3. hash the installed target.

### Generic staged-artifact commit

The commit path retains the observation before `before_temp_copy`, then runs the
expected-aware copy after that hook, and retains final target observation. It
falls from six reads to three while preserving both hook boundaries.

### Recovery

Recovery does not trust in-memory copy facts across a crash:

- an existing target is re-observed and compared with durable expected facts;
- an existing persisted temp is re-observed before installation;
- a missing temp uses the expected-aware fresh-copy path; and
- the installed target is re-observed.

No recovery state, commit point, or accepted collision changes.

### General copies

`copy_regular_file_checked` remains the self-contained primitive for callers
that do not have trusted expected facts. This change does not broaden the
optimization to backup, workflow terminal-artifact placement, or unrelated
filesystem copies.

## Preserved invariants

- Source and destination leaves must be regular files and must not be symlinks.
- The source is opened with no-follow semantics.
- Expected size and BLAKE3 must match before installation.
- Source metadata must be stable across the copy read.
- The temp and installed target are independently re-read and hashed.
- The target is installed by no-replace hard link.
- Target races never overwrite the winner.
- The file and both directory transitions retain their existing fsyncs.
- Owned temp/target cleanup remains fail-visible.
- A crash may leave only the same durable temp/target states accepted by current
  recovery.
- Audio publication remains relationally all-or-none.

ADR 0042 explicitly leaves malicious same-account writers and stronger
ownership, mode, parent identity, inode, and link-count fencing out of scope.
This change neither widens nor claims to close that threat-model boundary.

## Failure behavior

| Failure | Required result |
|---|---|
| source changed before or during copy | checksum mismatch; owned temp removed; no target |
| copied byte count/hash differs from expected | checksum mismatch; owned temp removed |
| destination write, flush, metadata, or fsync fails | commit/artifact error; owned temp cleanup attempted |
| temp changes before install | checksum mismatch; no target |
| target appears after preflight | no-replace failure; competing target untouched |
| installed target changes or reads incorrectly | checksum mismatch; owned target cleanup attempted |
| crash with exact persisted temp | recovery revalidates and installs it |
| crash with exact installed target | recovery revalidates it and removes exact temp alias |
| recovery sees mismatched temp or target | conflict; bytes retained as evidence |

## Verification

Behavior tests will cover:

- expected-aware copy returns exact source/destination facts;
- wrong expected size or hash removes the owned destination;
- changed staging facts prevent temp or target creation;
- temp mutation before install prevents publication;
- target appearance after preflight remains no-replace;
- successful promotion retains no temp and exact target facts;
- exact persisted-temp and installed-target recovery still succeeds;
- mismatched recovery temp/target remains fail-closed;
- generic staged-artifact commit failpoints retain durable recovery state; and
- plural audio extraction still publishes all members or none.

Focused commands:

```text
cargo test -p voom-control-plane artifact::fs::tests --all-features
cargo test -p voom-control-plane artifact::commit::tests --all-features
cargo test -p voom-control-plane audio::tests --all-features
cargo test -p voom-control-plane --test staged_artifact_flow --all-features
cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings
just ci
```

## Benchmark results

Pending prototype.
