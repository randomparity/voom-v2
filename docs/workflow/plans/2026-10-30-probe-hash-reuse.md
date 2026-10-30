# Probe hash reuse (#811) implementation plan

Goal: implement ADR 0120 contracts 2–9 per
[the spec](../specs/2026-10-30-probe-hash-reuse-design.md).

Architecture: three optional wire fields under protocol 6; a guarded branch in the ffprobe worker
(`guard.rs`) ahead of the unchanged recompute path; the scan pump sends the guard only with the
owner-node setting on and accepts `reused` only for a guarded request; the durable scan evidence
records the source.

Tech stack: Rust 2024 workspace, tokio, serde, `rustix` (Linux, `fs`), `libc` constants.

Expected implementation size: 550–800 changed lines (M) — file map below: about 250 production
lines across 12 files, 350–500 test lines, 40 doc lines.

## Global Constraints

- Toolchain from `rust-toolchain.toml`; workspace lints (`unsafe_code = "forbid"`, clippy pedantic,
  `unwrap_used`/`panic` denied outside `#[expect]`-annotated tests).
- New dependency: `rustix = { version = "1.1.4", default-features = false, features = ["fs", "std"] }`
  in `[workspace.dependencies]`, used as
  `[target.'cfg(target_os = "linux")'.dependencies] rustix.workspace = true` by
  `voom-ffprobe-worker` only. 1.1.4 is the version already locked; `cargo deny`/`cargo audit` must
  stay green.
- Unit tests in sibling `*_test.rs` files (ADR 0004). Tests use real time.
- Durable structs keep `#[serde(deny_unknown_fields)]`; new fields are
  `#[serde(default, skip_serializing_if = "Option::is_none")]` (ADR 0013).
- Keep these texts in `crates/voom-ffprobe-worker/src/handler.rs` byte-identical and unique:
  `use crate::{WorkerError, normalize_ffprobe_json, observe_file_facts};`, both
  `let pre_probe/post_probe = Box::pin(observe_file_facts(&path))` blocks, and
  `fn verify_expected_facts(` (`just media-evidence-candidates-selftest`).
- Builds and tests run under `flock -s /home/dave/src/voom-v2/.agent/corpus.lock`.
- Filesystem-dependent tests: tmpfs arms are `#[cfg(target_os = "linux")]` on
  `tempfile::tempdir_in("/dev/shm")`. The temp-root arm (`tempfile::tempdir()`, pinned to
  `.test-tmp/`) branches on `crate::guard::reuse_listed` of its `fstatfs` type on Linux: listed →
  reuse assertions, not listed → G5 fallback assertions; on non-Linux it always takes the
  fallback assertions. The hash-worker G4(b) test has no `statfs`; it asserts `/dev/shm` is tmpfs
  by reading `/proc/self/mounts` and runs the same body on the temp root, where detection does
  not depend on the filesystem type.
- Guard failure payload is exactly `{"stage": "pre_probe_guard"}` or `{"stage": "post_probe_guard"}`.

## File map

| File | Change |
|---|---|
| `crates/voom-core/src/taxonomy/scan_evidence.rs` (+`_test.rs`) | `HashSource`; `ScanObservationEvidence.hash_source` |
| `crates/voom-core/src/lib.rs` | `PROTOCOL_VERSION = 6`; export `HashSource` |
| `crates/voom-worker-protocol/src/operations/{hash_file,probe_file}.rs` (+tests) | `changed_at_ns`, `ProbeGuard`, `guard`, `hash_source`; export `ProbeGuard` |
| `crates/voom-worker-protocol/src/{wire/handshake_test,operations/dispatch_test}.rs` | version pins 5 → 6 |
| `crates/voom-hash-worker/src/hash.rs` (+`hash_test.rs`) | set `changed_at_ns: pre.changed_at_ns`; G4(b) tests |
| `crates/voom-ffprobe-worker/src/{guard.rs,guard_test.rs,lib.rs,observe.rs,handler.rs}`, `Cargo.toml` | guarded branch |
| `crates/voom-node-agent/src/{config,runtime,scan_session}.rs` (+tests, `tests/*.rs` literals) | setting, guard send, predicate |
| `crates/voom-node-agent/src/media.rs` (+`media_test.rs`) | `guard: None` fill-ins; version pin; G6 test |
| `crates/voom-test-support/src/scan_seed.rs`, `crates/voom-control-plane/src/scan/publish_test.rs`, `crates/voom-ffprobe-worker/tests/probe_worker.rs`, `crates/voom-node-agent/tests/container_audio.rs` | `None` literal fill-ins |
| `Cargo.toml`, `Cargo.lock` | rustix edge |
| `docs/payload-contract-inventory.md`, `docs/release-process.md`, `docs/runbooks/operator-node-agent.md`, `docs/media-evidence-decision-report.md` (lines 5–7) | docs |

## Task 1 — Wire and evidence types (protocol 6)

Files: the voom-core and voom-worker-protocol rows above, the hash worker row, and every literal
fill-in row (they must compile together).

Interfaces produced:

```rust
// voom_core (scan_evidence.rs, re-exported from lib.rs)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HashSource { Reused }
// ScanObservationEvidence: pub hash_source: Option<HashSource>
// voom_worker_protocol
pub struct ProbeGuard { pub changed_at_ns: i128, pub file_key: voom_core::FileKeyFacts }
// ProbeFileRequest: pub guard: Option<ProbeGuard>
// ProbeFileResult: pub hash_source: Option<voom_core::HashSource>
// HashFileResult: pub changed_at_ns: Option<i128>
```

Verification:

- Mode: focused-test — absent optional fields are omitted and old JSON still decodes:
  `probe_file_test.rs::unguarded_request_and_recomputed_result_keep_protocol_5_shape`,
  `hash_file_test.rs::result_round_trips_changed_at_ns`,
  `scan_evidence_test.rs::recomputed_evidence_encodes_without_hash_source` (golden string equal to
  the protocol-5 encoding) and `reused_evidence_round_trips`. Red: compile failure before the
  fields exist. Green: `cargo test -p voom-worker-protocol probe_file` and
  `cargo test -p voom-core scan_evidence`.
- Mode: focused-test — unknown guard field rejected:
  `probe_file_test.rs::guard_rejects_unknown_fields`.
- Mode: focused-test — version pin: `handshake_test.rs` asserts 6, renamed
  `protocol_version_is_six_for_probe_hash_reuse`; `dispatch_test.rs` message
  `schema 4 does not match protocol version 6`; `media_test.rs:626` string updated.
  Green: `cargo test -p voom-worker-protocol handshake dispatch`.
- Mode: focused-test — contract 2: `hash_test.rs::hash_result_reports_pre_read_ctime` (hash a
  file in a temp root, assert `changed_at_ns` equals `stat_facts(&metadata).changed_at_ns`).
  Red: field `None`. Green: `cargo test -p voom-hash-worker hash_result_reports`.

Steps: add types → fill `hash_source: None` / `guard: None` / `changed_at_ns` at every literal
(`rg -n "ProbeFileRequest \{|ProbeFileResult \{|HashFileResult \{|ScanObservationEvidence \{"`)
→ bump `PROTOCOL_VERSION` → run `cargo build --workspace --all-targets` → tests → commit
`feat(protocol): add probe guard, hash source and hash ctime under protocol 6`.

## Task 2 — Guarded probe in the ffprobe worker

Files: `crates/voom-ffprobe-worker/{Cargo.toml,src/guard.rs,src/guard_test.rs,src/lib.rs,
src/observe.rs,src/handler.rs}`, root `Cargo.toml`, `Cargo.lock`.

Interfaces: consumes Task 1 types. Produces
`pub(crate) async fn probe_reused(request: &ProbeFileRequest, guard: &ProbeGuard, config: &FfprobeConfig) -> Result<Option<ProbeFileResult>, FfprobeError>`
and `pub(crate) fn reuse_listed(f_type: i64) -> bool` (Linux). `observe.rs` makes
`open_regular_file_no_follow` and a `regular_file_metadata(path)` pre-check `pub(crate)`.

`handler.rs::probe_file` change (only additions above the anchors and `hash_source: None` in the
final literal):

```rust
    if let Some(guard) = &request.guard
        && let Some(result) = Box::pin(crate::guard::probe_reused(request, guard, config)).await?
    {
        return Ok(result);
    }
```

`guard.rs` outline: `fstatfs` via `rustix::fs::fstatfs(&std_file)`; `reuse_listed(t)` is
`t == libc::BTRFS_SUPER_MAGIC || t == libc::TMPFS_MAGIC`; `GuardFacts { size_bytes, modified_at:
Option<String>, changed_at_ns: i128, file_key: FileKeyFacts }` from `std::fs::Metadata`
(`MetadataExt::{ctime, ctime_nsec, dev, ino, nlink}`); `check(stage, facts, request, guard)`
returns `FfprobeError::ArtifactChecksumMismatch { payload: json!({"stage": stage}), message:
format!("{stage}: file facts differ from the reuse guard; no facts published") }`. Non-Linux:
`probe_reused` returns `Ok(None)`.

Verification (tests in `guard_test.rs`; tmpfs root `tempfile::tempdir_in("/dev/shm")`, asserted
tmpfs by `fstatfs`; the btrfs arm runs the same body on `tempfile::tempdir()` when that root is
btrfs, and the G5 arm runs on it when it is not listed):

- Mode: focused-test — reuse without reading: guard from the file's stat, `expected.content_hash`
  set to a hash of *other* bytes; result echoes it with `hash_source == Some(Reused)`
  (`guarded_probe_reuses_hash_without_reading_bytes`). Red: branch absent → recompute →
  checksum mismatch.
- Mode: focused-test — G4(a) before probe: same-size interior rewrite, mtime restored with
  `File::set_modified`, after a 20 ms wait → error class `ArtifactChecksumMismatch`, payload
  `{"stage":"pre_probe_guard"}`, message without the path
  (`guard_detects_restored_mtime_edit_before_probe`).
- Mode: focused-test — G4(a) during ffprobe: fake ffprobe script rewrites a byte with `dd
  conv=notrunc` and restores mtime with `touch -r <copy>` → `post_probe_guard`
  (`guard_detects_restored_mtime_edit_during_probe`).
- Mode: focused-test — G4(c): append-free write that moves mtime → `pre_probe_guard`
  (`guard_detects_mtime_moving_write`).
- Mode: focused-test — G5: on a non-listed root, result has `hash_source == None` and G4(a)'s
  stale expected hash fails as today's `pre_probe` mismatch
  (`guarded_probe_recomputes_outside_reuse_list`); `reuse_listed` truth table over btrfs,
  tmpfs, ext4 (`0xEF53`), NFS (`0x6969`), SMB2 (`0xFE534D42`) (`reuse_list_is_btrfs_and_tmpfs_only`).
- Mode: focused-test — no-follow: guarded probe of a symlink is `ArtifactUnavailable`
  (`guarded_probe_refuses_symlink`).
- Mode: focused-test — anchors intact: `just media-evidence-candidates-selftest`.

Green: `cargo test -p voom-ffprobe-worker guard`. Commit
`feat(ffprobe-worker): reuse the owner hash under a btrfs/tmpfs ctime guard`.

## Task 3 — Hash worker G4(b) on tmpfs and btrfs

File: `crates/voom-hash-worker/src/hash_test.rs`.

- Mode: focused-test — `restored_mtime_edit_during_hash_is_drift` on `/dev/shm` (Linux, asserted
  tmpfs via `/proc/self/mounts`) and on the temp root: `stat_facts` before, same-size rewrite + mtime restore, `stat_facts` after;
  `assert_stable` fails with payload `{"stage":"hash_drift"}`. This exercises #810's existing
  check on the gate filesystems; no production change. Green: `cargo test -p voom-hash-worker
  restored_mtime`. Commit `test(hash-worker): prove ctime drift detection on tmpfs and btrfs`.

## Task 4 — Owner-node setting and pump predicate

Files: `crates/voom-node-agent/src/{config.rs,runtime.rs,scan_session.rs}` and tests, plus
`AgentConfig`/`CoordinatorContext`/`ScanPumpContext` literals in `src/*_test.rs` and `tests/*.rs`.

Interfaces: `AgentConfig.probe_hash_reuse: bool` (`#[serde(default)]`);
`CoordinatorContext.probe_hash_reuse: bool`; `ScanPumpContext.probe_hash_reuse: bool`.

`probe_primary` builds `guard = context.probe_hash_reuse.then(|| ...)` from
`hash.changed_at_ns.zip(hash.file_key)`; after decode:

```rust
if result.hash_source.is_some() && guard.is_none() { return Ok(ProbeOutcome::Disagree); }
```

`ProbeOutcome::Snapshot` carries `(JsonValue, Option<HashSource>)`; the evidence literal sets
`hash_source`.

Verification (`scan_session_test.rs`, extending `ScriptedSingleFileWorker` with
`SingleFileBehavior::Reused` and `StreamEnd`, and `agreeing_hash_result` with
`changed_at_ns: Some(7)`):

- Mode: focused-test — setting off: recorded probe request has no `guard` key; evidence JSON has
  no `hash_source` (`setting_off_sends_no_guard_and_writes_no_reused_row`).
- Mode: focused-test — setting on: request carries `{"changed_at_ns":7,"file_key":{...}}`;
  a `Reused` result persists `"hash_source":"reused"` in `scan_observations.evidence_json`
  (`guarded_reuse_persists_hash_source`).
- Mode: focused-test — G7: setting off + worker answers `Reused` → evidence-less, counted failed
  (`reused_result_without_guard_is_refused`).
- Mode: focused-test — G7 mid-probe kill: setting on + probe stream ends before a terminal
  frame → lease `Failure(WorkerCrash)`, no evidence (`guarded_probe_crash_publishes_nothing`).
- Mode: focused-test — config: absent key parses `false`; `probe_hash_reuse = true` parses
  (`config_test.rs::probe_hash_reuse_defaults_off`).

Green: `cargo test -p voom-node-agent scan_session config`. Commit
`feat(node-agent): send the probe guard behind probe_hash_reuse and gate reused hashes`.

## Task 5 — Media probes stay unguarded; G6 and G8 gates

Files: `crates/voom-node-agent/src/media.rs` (`guard: None` at `child_request` and
`probe_output`, done in Task 1), `crates/voom-node-agent/src/media_test.rs`,
`crates/voom-ffprobe-worker/src/guard_test.rs`.

- Mode: focused-test — G6: `media_test.rs::restored_mtime_edit_before_transform_fails_checksum_mismatch`
  (write source, pin its hash, same-size rewrite with mtime restored, run → `Failure(
  ArtifactChecksumMismatch, ..)`, no child dispatch). Red: none expected (backstop already
  exists); bite check by pinning the post-edit hash, which must then dispatch.
- Mode: focused-test — G8: `guard_test.rs::unguarded_probe_sets_no_hash_source` (no guard on a
  tmpfs file → `hash_source == None` and the encoded result has no `hash_source` key).

Green: `cargo test -p voom-node-agent media` and `cargo test -p voom-ffprobe-worker unguarded`.
Commit `test: gate the transform backstop and unguarded probe shape`.

## Task 6 — Docs

`task-test-not-applicable`: prose with no executable consumer. Inventory rows 32 and 45 gain
the optional `hash_source` sentence; `release-process.md` gains a "Probe hash reuse / protocol 6
(ADR 0120)" note mirroring the protocol 5 note (drain schema-5 media work; setting stays off
through the swap; rollback is the setting until reused rows exist); runbook documents
`probe_hash_reuse`; report lines 5–7 say ADR 0120 is Accepted (2026-10-30) with #810 merged and
#811 implementing the probe guard. Commit `docs: record probe hash reuse setting and protocol 6`.

## Task 7 — Full guardrails

`just ci` under the corpus lock; record duration. Gates not executable in CI (for #794): G4/G5
on the operator's real btrfs corpus and network subset, G6 through a real control-plane transform,
G7 mixed-version agent/worker refusal on real hosts. G7 has no control-plane arm on the scan
path (no scan-batch version refusal exists; spec failure model).
