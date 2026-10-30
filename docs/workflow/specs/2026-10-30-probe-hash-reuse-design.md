# Probe reuses the owner hash under a local ctime guard (#811)

## Problem

ADR 0120 (Accepted 2026-10-30) lets the scan probe reuse the owner's BLAKE3 hash instead of
re-reading the file before and after ffprobe. #810 landed the 1 MiB buffer and the ctime
stability check. Still missing are implementation contracts 2 (the hash result reports the
pre-read ctime) and 3–9 of the [decision report](../../media-evidence-decision-report.md):
today `probe_file` in `crates/voom-ffprobe-worker/src/handler.rs` always hashes the whole file
twice, `ProbeFileRequest` has no guard, and the pump's agreement predicate
(`crates/voom-node-agent/src/scan_session.rs::agrees`) has no notion of a reused hash.

## Design

### Wire (one bump, `PROTOCOL_VERSION` 5 → 6)

- `voom_core::HashSource` (new, `scan_evidence.rs`): one variant `Reused`, serialized
  `"reused"`. Shared by the probe result and the durable evidence so both use one vocabulary.
- `HashFileResult.changed_at_ns: Option<i128>`: the pre-read stat's ctime ns, already held in
  `StatFacts.changed_at_ns`. Omitted when absent (non-Unix).
- `ProbeGuard { changed_at_ns: i128, file_key: FileKeyFacts }` (`deny_unknown_fields`) and
  `ProbeFileRequest.guard: Option<ProbeGuard>`. Size and mtime ns come from the existing
  `expected.size_bytes` and `expected.modified_at`; `format_iso8601` writes nine fractional
  digits, so string equality is nanosecond equality. The guard reuses `FileKeyFacts`
  (dev, ino, nlink) verbatim from the hash result; a link-count change also moves ctime, so
  comparing nlink adds no false change.
- `ProbeFileResult.hash_source: Option<HashSource>`.
- All three new fields are `#[serde(default, skip_serializing_if = "Option::is_none")]`, so an
  unguarded request and a recomputed result serialize exactly as in protocol 5.

### Probe worker

`probe_file` gains one branch before today's code: with a guard, call
`guard::probe_reused(request, guard, config)`. It returns `Ok(None)` when reuse does not apply,
and the unchanged recompute path runs. The recompute path's text stays byte-identical, so the
`single-read` variant anchors in `scripts/media-evidence-candidates.py` still apply.

`guard.rs` (new):

1. Open the path with the existing `symlink_metadata` regular-file check and `O_NOFOLLOW` open
   from `observe.rs` (made `pub(crate)`).
2. `rustix::fs::fstatfs` on that descriptor. Reuse only when `f_type` is `BTRFS_SUPER_MAGIC`
   (`0x9123683E`) or `TMPFS_MAGIC` (`0x01021994`). Any other type, or an `fstatfs` error,
   returns `Ok(None)` (recompute). Non-Linux targets always return `Ok(None)`.
3. Pre check on the open descriptor's `fstat`: size equals `expected.size_bytes`, mtime string
   equals `expected.modified_at`, ctime ns equals `guard.changed_at_ns`, `FileKeyFacts` equals
   `guard.file_key`. Any difference fails with stage `pre_probe_guard`.
4. Run ffprobe on the path, as today.
5. Post check: re-open the path the same way (so a file renamed over the path is caught by the
   file key) and repeat the comparison; a difference fails with stage `post_probe_guard`.
6. Return `pre_probe`/`post_probe` with the observed size and mtime and the echoed
   `expected.content_hash`, and `hash_source: Some(Reused)`.

A guard failure is `FfprobeError::ArtifactChecksumMismatch` with payload exactly
`{"stage": "<stage>"}` and a fixed message naming only the stage. No stat value, filesystem
type or path appears in either.

### Pump and agent

- `AgentConfig.probe_hash_reuse: bool` (`#[serde(default)]`, default `false`), the one
  owner-node setting, carried through `CoordinatorContext` into `ScanPumpContext`.
- `probe_primary` sends `guard: Some(..)` only when the setting is on and the hash result
  carries both `changed_at_ns` and `file_key`; otherwise `None`.
- Agreement (ADR 0077 as amended by ADR 0120): a result with `hash_source: Some(Reused)` from a
  request sent without a guard is `Disagree` (evidence-less). Otherwise `agrees` compares as
  today. The accepted source is copied into `ScanObservationEvidence.hash_source`.
- Media-child probes (`media.rs::child_request`) and output probes (`media.rs::probe_output`)
  send `guard: None`, so the worker recomputes and never sets `hash_source` for them. An
  agent-side refusal of a `hash_source` on those paths is outside the approved surface and is
  recorded as a follow-up candidate.

### Durable evidence

`ScanObservationEvidence.hash_source: Option<HashSource>` under ADR 0013 (additive, default
`None`, omitted when `None`). Readers: the batch route (`voom-api/src/scan.rs::typed_observation`,
unchanged code) and `scan_observations.evidence_json`. Publication ignores the field. Rows
written with recomputed hashes, and every output probe in `artifact_verifications.report`, stay
byte-identical. The inventory rows for both columns are updated.

### Dependency

`rustix` (`fs` feature) becomes a Linux-only normal dependency of `voom-ffprobe-worker`.
`unsafe_code = "forbid"` rules out `libc::fstatfs`, and std has no `statfs`. rustix 1.1.4 is
already in `Cargo.lock` through `tempfile`, so no new crate enters the build.

### Docs

`docs/payload-contract-inventory.md` (two rows), `docs/release-process.md` (protocol 6 flag-day
note, as for protocol 5), `docs/runbooks/operator-node-agent.md` (the setting), and the status
lines of `docs/media-evidence-decision-report.md` (lines 5–7 only).

## Failure model

1. **Actors and deployments**
   - Owner-node operator who sets `probe_hash_reuse` in the agent TOML.
   - Bundled, version-locked workers on loopback (ADR 0002, ADR 0016).
   - Other local processes or users that can write files under a storage root.
   - Deployments: Linux owner nodes with btrfs or tmpfs roots (reuse); every other filesystem
     and macOS (recompute).
2. **Invariants and assets at stake**
   - Published identity (`scan_observations.evidence_json` → publication) describes the bytes
     the owner hashed.
   - Fail-closed: any guard difference publishes no evidence.
   - Unguarded requests and recomputed rows are byte-identical to protocol 5.
   - Setting off ⇒ no guard sent ⇒ no reused row.
3. **Accepted failure classes**
   - Same-tick ctime edit, privileged ctime forgery, or damage preserving size, mtime and ctime:
     accepted residual named by ADR 0120 Consequences; transform input's full hash is the
     backstop.
   - A rename-swap in and back out of the path during ffprobe probes other bytes under an
     unchanged guard; today's two re-reads by path have the same window, so no new exposure.
   - A ctime-only change (chmod, xattr) between hash and probe fails closed; the next scan
     re-observes. Bounded cost.
   - `fstatfs` failure recomputes, the safe direction.
4. **Covered elsewhere**
   - Rollout measurement and real-storage gates G1–G3, G9 and the network-subset arm of G5: #794.
   - Transform input, output verification, promotion: unchanged (ADR 0120 table).
   - Protocol mismatch refusal: ADR 0016 handshake and operation-path middleware.

### Threat model

- **Boundaries added:** pump → probe worker `guard` (agent-built from the hash result);
  probe worker → pump `hash_source` (worker-asserted). **Widened:** durable evidence gains a
  field the API accepts from the agent.
- **Actors:** a local writer under the root (untrusted for content); the node agent and its
  bundled workers (trusted, same release, loopback with worker credentials); the
  authenticated node calling the batch route (trusted for its own root, as today).
- **Controls:** guard values are compared, never trusted as identity, and a mismatch fails
  closed; the pump accepts `hash_source` only for a guarded request; the worker sets it only on
  the guarded branch, which output and media-child probes never reach; failure payloads are fact-free; the guard path opens with
  `O_NOFOLLOW` after a regular-file check, as `observe_file_facts` does; serde
  `deny_unknown_fields` on every new struct; `ScanObservationEvidence::validate` still bounds
  size.
- **Out of scope:** a compromised node agent or bundled worker (it can already publish any
  facts); privileged forgery of ctime (accepted residual); network filesystems (never reuse).

## Success

- With the setting off, no probe request carries a guard and no evidence carries `hash_source`.
- With the setting on and a btrfs or tmpfs root, an unchanged file is probed without re-reading
  its bytes and its evidence carries `hash_source: "reused"`.
- Each G4 construction on tmpfs (and btrfs where the test root is btrfs) yields no evidence.
- On a filesystem outside {btrfs, tmpfs}, a guarded probe recomputes (no `hash_source`) and the
  G4(a) edit is detected.
- `just ci` is green.

## Validation

| Contract | focused-test | task-test-not-applicable |
|---|---|---|
| Hash result ctime (2) | hash worker: `hash_result_reports_pre_read_ctime` | — |
| Wire shapes (3, 9) | protocol: guard/source round trip, absent fields omitted, unknown field rejected; version pin 6 | — |
| Guarded reuse on tmpfs/btrfs (4) | ffprobe worker: `guarded_probe_reuses_hash_without_reading_bytes` | — |
| G4(a) between hash and probe | `guard_detects_restored_mtime_edit_before_probe` (tmpfs; btrfs where available) | — |
| G4(a) during ffprobe | `guard_detects_restored_mtime_edit_during_probe` (fake ffprobe edits) | — |
| G4(b) during hash read | hash worker: `restored_mtime_edit_is_drift_on_tmpfs` (+ btrfs where available) | — |
| G4(c) ordinary write | `guard_detects_mtime_moving_write` | — |
| G5 fallback | `guarded_probe_recomputes_outside_reuse_list` (filesystem of the test temp root when not listed) + `reuse_list_is_btrfs_and_tmpfs_only` | — |
| Fact-free failure (5) | guard tests assert payload equals `{"stage": ...}` and message has no path | — |
| Pump predicate (6), G7 | pump: setting off sends no guard; setting on sends guard; reused without guard is evidence-less; reused with guard persists `hash_source` | — |
| G7 mid-probe kill | pump: guarded probe stream ending early fails the session with no evidence | — |
| G6 transform backstop | node agent: restored-mtime edit before a transform fails `ARTIFACT_CHECKSUM_MISMATCH` with no dispatch | — |
| G8 unchanged readers (7) | evidence and probe-result encodings without the field match the protocol-5 bytes; an unguarded probe result carries no `hash_source` | — |
| Setting default (8) | config: absent key parses `false` | — |
| Docs and report status | — | prose; no executable consumer |

Not executable in CI, left to #794: G4 and G5 on the operator's btrfs corpus and network
subset, G6 end to end through a real control-plane transform, and G7's mixed-version fleet
refusal on real hosts (the handshake refusal itself is already tested).
