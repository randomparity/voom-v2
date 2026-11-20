# Extrapolate the control-plane and node-agent 16 KiB hashes — design (#889)

## Problem

#838 measured 16 KiB against 1 MiB on the four worker `observe_file_facts` loops. Two 16 KiB
hash buffers were not measured: `observe_regular_file` in
`crates/voom-control-plane/src/artifact/fs.rs` and `READ_BUFFER_BYTES` in
`crates/voom-node-agent/src/commit.rs`. The node-agent constant sizes two loops:
`try_observe_regular_file` (hash only) and `stream_copy_with_hash` (hash and copy).

## Scope

Decision per loop at `a793d4e0`, against the measured loop at `e2c29167`. The test: the same
`tokio::fs::File::read` into a 16 KiB heap `Vec<u8>`, `blake3::Hasher::update` on the filled
slice, and no other statement per chunk.

- **Control-plane `observe_regular_file`: extrapolate.** Its loop matches the measured one
  statement for statement; it differs only in error mapping.
- **Node-agent `try_observe_regular_file`: extrapolate.** Same loop; per-file differences
  only (plain `File::open` without `O_NOFOLLOW`, size and mtime recheck).
- **Node-agent `stream_copy_with_hash`: neither.** Each chunk also runs `write_all` to a temp
  file, and the copy ends with `flush` and `sync_all`, so the measured loop does not stand for
  it. It is private, and timing it needs a production visibility change outside this
  surface. Recorded as unmeasured; follow-up candidate to measure it before any change to the
  shared constant.
- The hash loops are not measured either: neither function is reachable from an example
  (`pub(crate) mod fs`; `pub(crate) fn`), and a replica of an identical loop re-measures #838.
- Disclosed limits: the production runtimes use tokio's default multi-thread runtime; the
  examples used 2 worker threads at 4 in flight. Both hand each read to the same blocking pool,
  so per-read cost does not depend on worker count. Call-site concurrency was not measured.
- Outputs: a section in `docs/media-evidence-baseline/observe/README.md` and the
  decision report's observe row and #838 not-measured line. No record, harness or code change.
  The extrapolated verdict is labelled extrapolated, never measured.

### Failure model

1. Actors and deployments: the maintainer reading the report before changing a buffer.
2. Invariants: no figure is presented as measured that was not; no production default
   changes; committed #838 records are untouched.
3. Accepted: production loop drift after `a793d4e0` voids the claim; the README names the
   commit and loops, and a buffer change re-checks them.
4. Covered elsewhere: the copy-loop measurement (follow-up candidate); the production buffer
   change and the network corpus (maintainer).

## Success

- The README and report name, for each of the three loops, its decision (extrapolated go,
  or unmeasured), the commit, and the structural reason.
- The report says a single `READ_BUFFER_BYTES` change also changes the unmeasured copy loop.
- `just source-checks` exits 0.

## Validation

- task-test-not-applicable: README section and decision-report row. Changed surface is
  prose; no executable consumer reads it, and the equivalence claim is about source at a
  named commit, re-checked by reading those lines (`fs.rs:171-184`, `commit.rs:864-877`,
  the copy loop at `commit.rs:732`, against the #838 loop).
- Guardrail, not a focused test: `just source-checks` (host-path check and the observe
  selftest, which validates the unchanged committed records).
