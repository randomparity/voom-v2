# Spec: Re-audit low-severity cleanup (issue #261)

Status: draft
Date: 2026-07-01
Issue: #261
Base ref audited: `fa5f16f`

## Context

Issue #261 is a grouped tracker of five LOW-severity findings from a read-only
re-audit. The trust model is single-operator / loopback, so these are hygiene /
defense-in-depth items, not shipping blockers. Each is small, independent, and
localized. None introduces a new architectural decision: every item conforms to
an **existing** ADR or an existing documented contract, so this spec adds no new
ADR (see "Decisions" below).

The issue also lists three "Observations (context, not necessarily action)".
They are explicitly out of scope for this change and are not addressed here.

## Items

### Item 1 — Recovery stat conflates absent vs unstattable

`crates/voom-control-plane/src/artifact/commit/recovery.rs:84`

**Current:** `let existing_target = observe_regular_file(&target_path).await.ok();`
collapses *every* failure — target absent (`NotFound`), permission denied, a
symlink or directory occupying the path, a transient IO error — into `None`,
which the downstream match reads as "target absent, resume a fresh install".

`observe_regular_file` returns `VoomError::ArtifactUnavailable(String)`, not a
raw `io::Error`, so the `io::ErrorKind` is not directly available at the call
site. Absence must therefore be classified by an explicit stat probe.

**Target:** Distinguish genuine absence from every other condition:

- Probe `tokio::fs::symlink_metadata(&target_path)`.
- `Err` with `kind() == NotFound` → target absent → resume fresh install
  (`existing_target = None`), unchanged from today.
- `Err` other kind (e.g. `PermissionDenied`) → propagate as
  `VoomError::CommitFailure` naming the target path; do **not** treat as absent.
- `Ok(_)` → the path is occupied; call `observe_regular_file(&target_path)` and
  propagate its result. A symlink / directory / non-regular file yields
  `ArtifactUnavailable`; a regular file yields facts fed to the existing
  matched-facts / mismatched-facts (`Conflict`) logic, unchanged.

This mirrors the `NotFound`-vs-other match already used by
`canonical_new_leaf_no_symlink` in the same module.

**Rationale / safety:** The downstream promotion uses a no-replace `hard_link`,
so no clobber was ever possible; the only defect is misclassification. The worst
case under the current code is a *misleading* downstream error ("artifact path
must not already exist", `CONFIG_INVALID`) or a spurious `CommitFailure` on a
retryable recovery. Surfacing the accurate reason makes recovery diagnosable and
lets a genuinely transient IO error be retried rather than mislabeled.

**Edge cases:**
- Absent target → repromote (unchanged behavior; regression-guarded).
- Already-installed matching file → finalize (unchanged; regression-guarded).
- Installed mismatched-facts file → `Conflict` (unchanged; regression-guarded).
- Directory / symlink at target → the probe returns `Ok(_)` and delegates to
  `observe_regular_file`, yielding a loud `ArtifactUnavailable`, not a
  fresh-install attempt that fails with a misleading message.
- Non-`NotFound` stat error (permission denied, or an intermediate path
  component that is not a directory → `ENOTDIR`) → `CommitFailure` naming the
  path. This is the branch that implements the fix's core distinction.
- Probe → observe TOCTOU: a target that vanishes between the `symlink_metadata`
  probe and `observe_regular_file` surfaces as `ArtifactUnavailable` rather than
  repromoting. Erroring is acceptable — recovery is idempotent and retryable.

**Acceptance criteria:**
- The `Err(kind != NotFound)` arm is exercised deterministically: recovery with
  an intermediate path component replaced by a regular file (so the target stat
  fails with `ENOTDIR`, not `NotFound`) returns `ErrorCode::CommitFailure` and
  does not attempt a fresh install. This is the primary new-behavior test and
  needs no permission/uid manipulation, so it runs the same on CI and locally.
- Existing recovery tests (`recover_commit_repromotes_when_target_absent`,
  `recover_commit_resumes_finalize_when_target_already_installed`) still pass,
  guarding the `NotFound`→absent and matching-facts→finalize paths.

### Item 2 — Client never validates handshake `agreed`

`crates/voom-worker-protocol/src/http/client.rs` (`handshake`)

**Current:** On a 2xx response the client deserializes `HandshakeResponse` and
returns it without checking `agreed`. The server is the authority (only returns
200 on exact match), so it is structurally safe today, but the server→client
direction has no defense-in-depth.

**Target:** After decoding a 2xx `HandshakeResponse`, reject it when
`agreed != offered` with `ProtocolError::UnsupportedProtocolVersion { offered,
expected: agreed }`. The exact-match contract (ADR-0016) means the only valid
echo is `agreed == offered`.

**Edge cases:** matching echo → `Ok`; mismatched echo → `UnsupportedProtocolVersion`;
malformed body → existing `InvalidPayload` decode path (unchanged); non-2xx →
existing error-decode path (unchanged).

**Acceptance criteria:** A fake server returning 200 with `agreed != offered` is
rejected with `UnsupportedProtocolVersion`; a matching echo still succeeds.

### Item 3 — Fake worker re-implements the version check inline

`crates/voom-fakes/src/bin/chaos_worker.rs` (`enforce_version`)

**Current:** `enforce_version` hand-rolls the exact-match check while the
handshake path already delegates to `voom_worker_protocol::negotiate`. This
reintroduces the two-copies pattern ADR-0016 eliminated in production; a future
semantics change could drift this copy while conformance stays green.

**Target:** Delegate the operations-path check to
`voom_worker_protocol::negotiate` (mapping `Ok(_) → Ok(())`), matching the
handshake path. Behavior is identical today — both return
`UnsupportedProtocolVersion { offered, expected }` on mismatch and both require a
present, parseable header — so this is a de-duplication with no observable change.

**Acceptance criteria:** `cargo build -p voom-fakes` succeeds; the Chaos
Librarian conformance/E2E behavior is unchanged (wrong version still rejected,
missing header still rejected).

### Item 4 — Malformed version header reported as "missing"

`crates/voom-worker-protocol/src/http/server.rs` (`enforce_version`)

**Current:** `headers.get(...).and_then(to_str).and_then(parse::<u32>().ok())`
collapses a present-but-unparseable header (`"1.0"`, `"abc"`, overflow) to
`None`, which is reported as `InvalidPayload { detail: "missing …" }`. The
request is still rejected loudly; only the detail string misattributes a
malformed value as absent.

**Target:** Separate the two conditions:
- Header absent (or non-ASCII / not `to_str`-able) → `InvalidPayload { detail:
  "missing <header>" }` (unchanged).
- Header present but not a `u32` → `InvalidPayload { detail: "malformed
  <header>: <value>" }`.

Redact nothing further: the protocol-version header is not a secret and echoing
it aids diagnosis. A present value is still routed through `negotiate` when it
parses.

**Acceptance criteria:** An absent header still yields a "missing" detail; a
present unparseable header yields a "malformed" detail; both remain
`InvalidPayload` and both are rejected.

### Item 5 — Builtin-worker ensure uses a deferred `BEGIN`

`crates/voom-control-plane/src/transcode/commit.rs` and sibling `ensure_*`
callers.

**Current:** Five call sites bootstrap a builtin worker under a deferred
`pool.begin()` / `begin_tx`, then run a check-then-insert
(`get_by_name_in_tx` → `register_in_tx` / capability / grant):

- `transcode/commit.rs` `ensure_result_probe_worker`
- `remux/commit.rs` `ensure_result_probe_worker`
- `audio/commit.rs` `ensure_result_probe_worker`
- `scan/mod.rs` `ensure_scan_worker`
- `artifact/verify.rs` (verify-artifact worker ensure + event append)

`workers.name` is `UNIQUE`, so duplicates are impossible; the only failure mode
is a spurious unique-violation / `SQLITE_BUSY` if two commits race, because a
deferred `BEGIN` acquires the write lock lazily and SQLite returns `SQLITE_BUSY`
*without* invoking the busy handler on a lock upgrade.

**Target:** Switch these read-then-write bootstraps to `begin_immediate_tx`,
matching the remote-execution family. `BEGIN IMMEDIATE` takes the write lock up
front so `busy_timeout` serializes racing writers cleanly. Use the existing
`crate::cases::begin_immediate_tx` helper for consistency rather than inlining
`begin_with`.

**Rationale / safety:** Semantics are unchanged for the single-writer common
case; the change only affects contended concurrent bootstraps, converting a
spurious failure into a serialized wait. Write-lock hold time barely changes: in
every one of the five sites the first write (`register_in_tx` inside `ensure_*`,
or the immediately-following `append_event` in `verify.rs`) occurs at most one
`get_by_name_in_tx` SELECT after `begin`, so `BEGIN IMMEDIATE` grabs the lock
essentially where the deferred `BEGIN` would have anyway. None of these five is
a long-running transaction. This matches the already-accepted remote-execution
family. The shared helper's error context ("begin immediate") replaces the
per-site descriptive context; begin failures are effectively unreachable (dead
pool) and the message detail is not a public contract (only `code` strings are).

**Acceptance criteria:** All five sites use `begin_immediate_tx`; existing
transcode/remux/audio/scan/verify tests still pass; `just lint` clean.

## Decisions

- **No new ADR.** Items 2–4 conform to ADR-0016 (exact-version match,
  `negotiate` as the single source of truth). Item 5 applies the contention
  pattern already documented on `begin_immediate_tx`. Item 1 conforms to the
  existing recovery/commit failure contract and mirrors an existing
  `NotFound`-vs-other idiom in the same module. None changes a layer boundary,
  interface split, concurrency invariant, or migration.
- **Direct implementation, not subagent fan-out.** The five items are small and
  independent but share one expensive workspace build (`just test` builds all
  targets `--all-features`). Sequential direct implementation with per-item TDD
  is equally rigorous and avoids paying that build cost five times. All work
  stays on one feature branch; no parallel mutating agents.

## Out of scope

The three "Observations" in the issue (two >1,000-line leaf files;
best-effort `let _ = fail_job/succeed_job` discards; `header_read_timeout`
h1-only) are context, not action items, and are not touched here.
