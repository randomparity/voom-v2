---
status: accepted
date: 2026-07-02
deciders: [VOOM core]
---

# 0024 — Malformed-media failure class and hardlink inode facts

## Context

Two robustness gaps surface when a scan/execute runs over thousands of real,
heterogeneous files (#287, closing #248 and #249):

- **Corrupt input is misclassified as transient.** When ffprobe/ffmpeg reject a
  file because its *bytes* are malformed, the workers map the non-zero exit to
  `FailureClass::ExternalSystemUnavailable` — a **retriable** class. A
  permanently-broken file therefore looks retriable forever: the control plane
  keeps re-leasing work that can never succeed. There is no permanent
  "the input is the problem" class; the closest, `MalformedWorkerResult`, means
  "the *worker's own result* was malformed", not "the source media is corrupt".
- **Hardlinks are indistinguishable from copies.** Scan records content hash and
  size but no inode facts. `record_discovered_file_in_tx` mints a fresh
  `file_asset` per discovered path unless given an alias proof, so two hardlinks
  to one physical file ingest as two independent assets. A later mutation/commit
  on one asset silently alters the other physical bytes, and dedup cannot tell a
  hardlink (one physical file) from a byte-identical copy (two physical files).

A third, smaller gap — no mid-run progress visibility — is handled by a view
field and runbook text and needs no architectural decision; it is specified but
not an ADR concern.

## Decision

### 1. Add a permanent `MALFORMED_MEDIA` failure class

Introduce `FailureClass::MalformedMedia` (non-retriable partition) and a 1:1
`ErrorCode::MalformedMedia` (`MALFORMED_MEDIA`). The ffprobe and ffmpeg workers
classify a **non-zero process exit** by matching the tool's own stderr against a
conservative, curated set of "bad input" diagnostics: a match is
`MalformedMedia` (permanent); every other non-zero exit, spawn failure, signal
kill, or timeout stays `ExternalSystemUnavailable` (transient). The match is
deterministic substring matching, not a model judgment (AGENTS Rule 6), and is
tuned for **precision over recall**: a missed signature degrades to the prior
retriable behavior (no regression), whereas a false positive would wrongly
condemn a transient failure as permanent.

No `VoomError::MalformedMedia` variant is added: the class only ever travels on
the worker `ProgressFrame::Error` (class + code), never as a control-plane
error value. The `voom-api` exhaustive `ErrorCode` match gains a `MalformedMedia`
arm in the shared default group solely to remain exhaustive.

The directory-scan per-file continuation guard (`is_ffprobe_exit`, renamed
`is_unprobeable_media`) is widened to admit `MalformedMedia`, so a corrupt file
in a large scan records a per-file failure and the run proceeds (#213 behavior),
now with an honest permanent class.

### 2. Record hardlink inode facts and resolve them at ingest

Capture `(st_dev, st_ino, nlink)` during the existing scan stat and persist them
in a new additive `scan_file_facts` table (migration 0017), one row per ingested
local `file_location`, with a `(dev, ino)` index. At ingest, a candidate whose
`(dev, ino)` matches a live prior local location **and whose `(content_hash,
size)` equals that location's `file_version`** is a **hardlink**: its path is
attached as an additional live `file_location` on the *existing* `file_version`
(routed through a repo method that consults the pending-commit lock, exactly as
alias-attach does), and no new asset/version/snapshot is minted. A candidate
with no `(dev, ino)` match — including a byte-identical **copy**, which has a
different inode — ingests as a new asset as before. The content-hash+size
equality is a required integrity guard: filesystems recycle inode numbers and
files can be edited in place (same inode, new bytes), so a `(dev, ino)` match
alone would let a recycled inode or an in-place edit silently attach mismatched
bytes to a stale version; on a hash mismatch the candidate falls through to
normal ingest. `(dev, ino)` is the physical-object key; content hash remains the
dedup-candidate signal and, here, the attach integrity guard.

## Consequences

- Corrupt files now terminate as `MALFORMED_MEDIA` and are not retried; operator
  issues opened from them carry non-retriable `High` severity/priority.
- The failure taxonomy grows from 22 to 23 classes; the conformance registry and
  taxonomy tests are extended in lockstep (exhaustive matches force this).
- `MALFORMED_MEDIA`/`MalformedMedia` become public wire contract (error code and
  failure-class strings) and additive durable data (`scan_file_facts`); both are
  additive-only per the schema-evolution contract (ADR 0013).
- Two hardlinks share one `file_version`, so the commit safety gate's closure
  sees both paths as one identity and cannot double-process them. `nlink` is
  recorded for visibility.
- Migration 0017 is applied only by `voom init` (ADR 0003); `connect` never
  migrates.
- The `#[ignore]` chaos characterization test is rewritten to the new behavior;
  CI coverage comes from store + control-plane tests that do not need external
  media tooling.

## Considered & rejected

- **Reuse `LocationProof::LocalFileIdGeneration` to carry inode identity instead
  of a new table.** The proof value is an opaque JSON blob, not indexable by
  `(dev, ino)`, stores no `nlink`, and conflates macOS/Windows
  file-id/generation semantics with a Linux `(dev, ino)`. A dedicated indexed
  table is queryable, records `nlink`, and keeps the physical-object key
  explicit. (The alias-attach *protection* — the pending-commit lock — is still
  reused.)
- **Add inode columns directly to `file_locations`.** Wider blast radius on a
  hot, heavily-tested table that a sibling PR (#278) is also touching; a
  separate table is strictly additive and isolates the change.
- **Classify malformed media by process exit code alone.** ffmpeg/ffprobe return
  exit code 1 for both corrupt input and many transient failures, so the exit
  code cannot separate them; the stderr diagnostic is the only reliable signal.
- **A retry-budget on `ExternalSystemUnavailable` instead of a new class.** That
  hides a permanent fault behind exhausted retries and still reports the wrong
  (transient) class; the taxonomy should name the real fault.
- **Model-classify the failure text.** Deterministic and auditable string
  matching answers this; a model call would be non-reproducible and violate
  AGENTS Rule 6.
