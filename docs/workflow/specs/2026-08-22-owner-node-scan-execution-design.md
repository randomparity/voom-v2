# Owner-node scan execution — design (issue #421)

Status: draft → reviewed
ADR: [0077](../../adr/0077-owner-node-scan-execution.md)
Branch: `feat/owner-node-scan-execution-421` off `main`

## Outcome

Move scan discovery, hashing, and probing to the storage-owner node agent so the control
plane never opens discovered bytes. Scan runs are ticket-routed (`scan_library` tickets carry
the durable `scan_session_id`); the agent streams ordered, idempotent, evidence-bearing
observation batches into the existing ADR 0067 session substrate; identity publishes only
from agreed hash+probe evidence at completion; every control-plane-local byte path is removed.

## Acceptance criteria (issue #421, binding)

1. Discovery, hash, and probe succeed when the control plane cannot access the root.
2. File mutation during hash/probe fails without publishing stale facts.
3. Symlink escape, malformed locator, leading-dash path, unstable file, hardlink, empty root,
   and large batch cases are covered by tests.
4. Separate scan/hash capabilities and grants are enforced.
5. The old control-plane discovery/hash implementation and direct path dispatch are removed.

## Architecture

Three tiers already exist; this change supplies the missing producer and removes the
transitional consumer.

```
CLI voom scan --root N
  └─ CP request_scan_run(root): session(requested) + ready scan_library ticket
       payload: WorkflowTicketPayload { rendered_payload: {scan_session_id,
       storage_root_id}, declared_artifact_access: read(root) }
  owner node agent acquires lease (existing ADR 0070/0072 gating proves ownership)
  └─ scan-session pump (agent runtime)
       ├─ POST /v1/scan/node/{n}/session/{s}/start          (client method added)
       ├─ voom-scan-worker   ScanLibrary   → candidate progress frames
       ├─ per candidate:
       │    voom-hash-worker  HashFile     → observed facts | Error(drift)
       │    voom-ffprobe-worker ProbeFile  → snapshot        | Error(malformed)
       ├─ POST .../batch/{seq}  (≤1000 observations, idempotency key, ordered)
       └─ POST .../complete | .../fail      → settle lease Complete|Fail
  CLI polls cp.scan_session until terminal → outcome envelope
```

### Components

**C1 — Evidence field (migration 0041).**
`migrations/0041_scan_observation_evidence.sql`:
`ALTER TABLE scan_observations ADD COLUMN evidence_json TEXT;` plus a CHECK that it is NULL
or valid JSON when non-null (mirroring existing strict-table style). Domain
`ScanObservation` (voom-store) gains `evidence: Option<ScanObservationEvidence>`;
wire `ScanObservationRequest` (voom-api) gains the same optional field with
`#[serde(default)]`. New type `ScanObservationEvidence` in voom-core
(`taxonomy/scan_evidence.rs`), `deny_unknown_fields`:

```rust
pub struct ScanObservationEvidence {
    pub content_hash: String,            // "blake3:<hex>" as hashed on the node
    pub size_bytes: u64,
    pub modified_at: String,             // RFC 3339
    pub file_key: Option<FileKeyFacts>,  // { dev, ino, nlink } u64s (Unix)
    pub sidecars: Vec<ScanSidecarEvidence>,
    pub probe_snapshot: serde_json::Value,
}
pub struct ScanSidecarEvidence {
    pub provider_relative_locator: String,
    pub role: String,                    // external_subtitle|nfo|poster|trailer
    pub sha256_hex: String,
    pub size_bytes: u64,
}
```

Registered in `docs/payload-contract-inventory.md` and
`scripts/payload-contract-scope.txt`. Batch acceptance stores it verbatim after structural
validation; no business classification at the boundary beyond existing checks.

**C2 — Worker contracts (voom-worker-protocol).**
`operations/scan_library.rs`:

```rust
pub struct ScanLibraryRequest { provider_locator: String, extension_allowlist: Vec<String> }
pub struct ScanCandidate { primary: ScanCandidateFile, sidecars: Vec<ScanCandidateFile> }
pub struct ScanCandidateFile {                       // deny_unknown_fields
    pub provider_relative_locator: String,           // validated ProviderRelativeLocator shape
    pub provider_object_identity: String,            // "dev=…;ino=…" stat identity string
    pub size_bytes: u64,
    pub modified_at: String,
    pub kind: Option<String>,                        // sidecar role; None for primaries
}
pub struct ScanLibraryResult { discovered_count: u64, skipped_count: u64 }
```

Progress frames carry `payload: {"candidates": [...≤256...]}` (strict decode). Result frame
carries `ScanLibraryResult`.
`operations/hash_file.rs`:

```rust
pub struct HashFileRequest { provider_locator: String, provider_relative_locator: String }
pub struct HashFileResult {
    pub content_hash: String, pub size_bytes: u64, pub modified_at: String,
    pub file_key: Option<FileKeyFacts>,
    pub stability_started_at: String, pub stability_confirmed_at: String,
    pub sidecars: Vec<HashedSidecar>,   // locator + role + sha256_hex + size_bytes
}
```

Registry entries added for both operations.

**C3 — Bundled workers.**
New crates `voom-scan-worker`, `voom-hash-worker` following `voom-ffprobe-worker`'s exact
shape (`load_worker_credentials_from_env`, `serve_worker_http`, `BOUND addr=` line, stdin
watchdog). Shared pure classification logic (extensions, sidecar kinds/roles, longest-stem
matching, allowlist filter) moves from `voom-control-plane/src/scan/discovery.rs` to
`voom-scan-worker/src/discover.rs`. Policy enforcement:

- canonicalize root once; walk with `read_dir`, skipping symlinks (`file_type().is_symlink()`
  ⇒ skipped, counted); reject any candidate whose joined path escapes the canonical root;
- relative locators built from components joined with `/`; each component checked against
  `.`/`..`/empty/NUL (defense-in-depth beneath `ProviderRelativeLocator::new`);
- hash worker resolves `canonical_root + relative_locator` component-wise using
  `openat`-equivalent semantics via `std::os::unix::fs::OpenOptionsExt` `O_NOFOLLOW` per
  component descent (`openat2::ResolveBeneath` not required; a manual loop opening each
  component with `O_NOFOLLOW|O_DIRECTORY` and finally the file with `O_NOFOLLOW` suffices);
  stat before hash, re-stat after; any difference ⇒ terminal `Error` frame
  (`FailureClass::ContentDrift`) with no facts.
- probe paths handed to ffprobe are always absolute (canonical root join), never starting
  with `-`; the ffprobe worker additionally receives them unchanged (no argv injection is
  possible through argv arrays, but absolute-path reconstruction is asserted in tests).

**C4 — Agent scan-session pump (voom-node-agent/src/scan_session.rs).**
Client methods live in a new file `crates/voom-node-agent/src/scan_client.rs` implementing
`ControlPlaneClient` inherent methods (`start_scan_session`, `submit_scan_batch`,
`complete_scan_session`, `fail_scan_session`) over the existing `send` envelope transport
with node token + idempotency keys — no edits to `client.rs` bodies (parallel-work isolation
with sibling #422).
Pump behavior for a `scan_library` dispatch:

1. Decode `WorkflowTicketPayload`; take `rendered_payload.scan_session_id`.
2. `start_scan_session` → session status must become `running`; record deadline.
3. Dispatch `ScanLibrary` to the worker child; consume candidate frames.
4. For each candidate, bounded pipeline (JoinSet, ≤4 in flight): `HashFile` then `ProbeFile`
   (expected = hash facts; verify pre/post match). Outcomes: agreed ⇒ observation WITH
   evidence; drift/malformed/unreadable ⇒ observation WITHOUT evidence (existence recorded);
   vanished mid-run ⇒ no observation (absence is real).
   Agreement predicate (exact fact set, evaluated in the pump): hash worker's post-read stat
   equals its pre-read stat AND ffprobe `pre_probe`/`post_probe` each match the hash result's
   {size_bytes, content_hash, modified_at}. Probe paths are absolute canonical-root joins
   (never option-like); probing stays pathname-based until #423 reference-passing.
5. Buffer observations; flush a batch whenever ≥1000 accumulate or enumeration ends.
6. After scan `Result`: flush remainder, `complete_scan_session(last_sequence,
   observation_count)`; lease settles `Complete` with summary `{scan_session_id,
   observed_count, published: unknown-at-this-tier}` — actually summary carries only counts
   known agent-side (`observed_count`, `failed_content_count`, `skipped_count`).
7. Fatal errors (worker crash, protocol error, CP unreachable after retries, batch conflict):
   best-effort `fail_scan_session(reason)`; lease settles `Fail`.

Runtime wiring: `run_lease` checks `dispatch.operation == "scan_library"` and routes to the
pump instead of plain `dispatch_to_child`. This is the one deliberate edit inside
`runtime.rs`; kept to a single match arm calling `scan_session_pump(...)`.

**C5 — Control plane: run request + completion publication.**
`ControlPlane::request_scan_run(root_id, idle_timeout_seconds)` in
`crates/voom-control-plane/src/scan/run.rs`: effective-root lookup, availability fail-close,
`insert_requested_in_tx` (owner node + epoch from the root), `create_ticket_in_tx` with
kind `scan_library`, payload encoded via `WorkflowTicketPayload` (`workflow_id:
"scan-run"`, `plan_id: scan_session_id.to_string()`, `node_id: owner`,
`branch_id: "scan-run-{session}"`, `timing: EffectiveTiming { duration_ms:
idle_timeout_seconds*1000, progress_interval_ms: … }`), `declared_artifact_access` =
`declaration_for(ScanLibrary, Root{storage_root_id})`, then `mark_ready_if_unblocked`.
Returns `{scan_session_id, ticket_id}`.
Completion publication: `complete_scan_session` case extended — inside its existing
`BEGIN IMMEDIATE` transaction, before `complete_in_tx` retirement, publish identities from
evidence-bearing observations of this session: same-address replay check, hardlink attach
via `(dev, ino)` facts, fresh ingest minting asset/version/location + events, media snapshot
from `probe_snapshot`, sidecar bundle membership from sidecar evidence (roles preserved),
inode scan-fact recording. Logic relocates DB-only from `scan/persist.rs` (which loses all
byte reads); `verify_probe_facts` semantics move into the pump's agree/disagree rule.

**C6 — Removal.**
Delete: `scan/discovery.rs`, `scan/hash.rs`, the `scan/mod.rs` byte pipeline (`scan_path*`,
grouping, launcher/classifier traits), old `scan/library.rs` filesystem checks. KEEP
`scan/worker.rs` and `scan/bootstrap.rs`: audio/remux/transcode commit probing, policy tool
preflight, and artifact verification still consume them (#423/#424 surfaces). CLI direct
dispatch rewritten to request+poll (`--no-wait` skips polling); `VOOM_FFPROBE_BIN` warning
replaced by nothing.
Tests deleted/moved with their subjects; `check-test-layout` keeps siblings co-located.

## Data flow guarantees

- Batches strictly sequential sequence numbers starting at the value returned by start
  (`next_sequence`), one in-flight batch, idempotency key derived from
  `{session, sequence}` so retries replay instead of duplicating (server ledger replays
  accepted outcomes).
- Observations within a batch ordered by discovery order; locators unique per session
  (server-enforced).
- Completion watermark = last flushed sequence; empty traversal completes with
  `last_sequence: null`, retiring pre-start locations (criterion: empty root).

## Threat model

Boundaries added/widened:

| Boundary | In | Actor | Control |
|---|---|---|---|
| Ticket payload → agent | session/root IDs, root path | authenticated owner node | payload strict-decoded twice (CP encode, agent decode); declaration resolution proves the acquiring node owns the root |
| Agent → children | root path, relative locators | local child (untrusted-ish) | children get agent-generated credentials, loopback bind; payloads contain no CP bearer token |
| Workers → filesystem | untrusted filenames/paths | local writer of media files | component-wise O_NOFOLLOW descent, escape rejection, symlink skip; leading-dash neutralized by absolute-path construction |
| Node → session routes | observations/evidence JSON | authenticated owner node | existing incarnation fence + strict deny_unknown_fields decode + 100k cap + contiguous-sequence rule (unchanged) |

Out of scope: malicious *owner* node fabricating evidence about its own roots (it holds the
bytes by definition — trust follows ADR 0050); #423's reference-passing for transform
workers; cross-provider (non-filesystem) roots.

No AI surface; no eval plan required.

## Test plan (maps to acceptance criteria)

1. CP-unreachable-root: integration test runs `request_scan_run` against a root whose
   `provider_locator` does not exist on the control-plane host (tempdir removed there /
   never created) while a test agent with workers pointed at the real tempdir completes the
   session; assert `succeeded` and locations published.
2. Mutation during hash: seed file, make hash observe mutated bytes between stats (test
   hook swaps content via a second writer after first stat — deterministic via a
   controllable `Clock`/pre-post stat spy in the worker unit test); assert no evidence
   published and location (if pre-existing) not retired.
3. Edge matrix (worker unit tests): symlink escape (leaf symlink out of root rejected;
   ancestor-swap regression per debt 0004: rename dir then place symlink — covered by
   component-wise descent test), malformed locator (batch route rejects; agent drops with
   count), leading-dash filename scanned/probed safely, unstable file ⇒ evidence-less
   observation, hardlink pair attaches to one asset two locations, empty root retires
   pre-start locations, 2500 candidates split across ≥3 batches under the 1000 limit.
4. Grants: activation of an agent declaring `[scan_library]`-only worker cannot acquire a
   `hash_file` ticket (`WorkerIneligible`); vice versa.
5. Removal: workspace compiles with deleted modules; grep gate test asserts no
   `discover_path_filtered`/`observe_candidate_file_in_root` symbols remain.

Guardrails: focused `cargo test -p <crate>` during development; full `just ci` before push.
