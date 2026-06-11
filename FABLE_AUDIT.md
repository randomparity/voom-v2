# VOOM — Fable Audit Report

**Date:** 2026-06-10
**Auditor:** Claude Fable 5 (principal-level technical audit)
**Scope:** Full workspace at `main` @ `b766b9c`
**Mode:** Analysis only. No code was changed.

> **Calibration note.** Findings below were produced by parallel per-crate
> exploration and then spot-verified against source. Where a sub-audit
> over-stated severity, I have downgraded it and said so explicitly. Severities
> reflect VOOM's actual trust model: it is a **pre-release, single-operator,
> loopback-only** control plane. The worker protocol's trust boundary is the
> control plane that spawns the workers — *not* a remote attacker — so several
> "security" items are really **defense-in-depth / robustness** items, marked as
> such. Treat anything tagged **[unverified]** as a hypothesis to confirm with a
> targeted test, not a settled defect.

> **Verification pass (2026-06-10, against source @ `b766b9c`).** Every finding
> below was independently re-checked against the actual code at the cited
> locations. Verdicts are annotated inline as **[VERIFIED]**, **[VERIFIED w/
> correction]**, or **[REFUTED]**. Headline outcomes: two findings are **refuted**
> (M3, M8); L10 **miscategorized** `planner.rs` as a control-plane module (it
> lives in `voom-plan` at 1,166 lines, not 1,138);
> M12's "exempt from the body-size cap" claim is **false**; and **M6's downgrade
> rationale was backwards** — the acquire transaction uses a *deferred* `BEGIN`,
> so SQLite write serialization does **not** eliminate the race, and M6 is raised
> back toward High. No new critical defects were found beyond the originals; the
> genuinely load-bearing correctness bugs are **M5, M6, M7**.

---

## 1. What VOOM is

A control-plane-first Rust application for managing video libraries: it parses a
policy DSL, generates deterministic execution plans, routes work through durable
SQLite-backed tickets/leases, and executes media operations via out-of-process
worker binaries (ffmpeg / ffprobe / mkvtoolnix / verify) speaking a versioned
HTTP+NDJSON protocol over loopback. The agent-facing surface is the `voom` CLI,
which emits exactly one JSON envelope per invocation.

**Stack:** tokio + sqlx 0.8 (SQLite) + axum, async-first, Rust 2024 edition,
`rust-version = 1.95`. `unsafe_code = forbid`; clippy pedantic on; panic / unwrap
/ print denied at the workspace level.

### Size and shape

| Crate | Lines | Src files | Role |
|---|---:|---:|---|
| `voom-control-plane` | 58,539 | 141 | App-services + workflow orchestration (largest) |
| `voom-store` | 38,966 | 62 | SQLite pool, migrations, repositories |
| `voom-cli` | 12,100 | 35 | `voom` binary — primary entry point |
| `voom-plan` | 8,208 | 33 | Deterministic planning, compliance, plan hashing |
| `voom-policy` | 5,915 | 34 | Policy DSL parser / validate / compile |
| `voom-ffmpeg-worker` | 5,355 | 11 | ffmpeg transcode / audio worker |
| `voom-events` | 5,262 | 27 | Event envelope + payload taxonomy |
| `voom-worker-protocol` | 5,109 | 34 | HTTP/NDJSON worker contract |
| `voom-fakes` | 3,402 | 18 | Fake / chaos / benchmark workers |
| `voom-core` | 3,029 | 34 | Shared types, errors, IDs, Clock |
| `voom-mkvtoolnix-worker` | 3,021 | 11 | mkvmerge remux worker |
| `voom-conformance` | 2,867 | 14 | Black-box protocol conformance harness |
| `voom-ffprobe-worker` | 2,009 | 9 | ffprobe probe worker |
| `voom-fake-support` | 1,739 | 8 | Fake-provider runtime |
| `voom-api` | 1,448 | 3 | axum router (no server binary yet) |
| `voom-verify-artifact-worker` | 797 | 7 | Artifact verification worker |
| `voom-scheduler` | 631 | 2 | Worker scoring / selection |
| `voom-test-support` | 442 | 3 | Integration-test support |
| `voom-artifact` | 118 | 2 | (was "reserved" — now holds commit-pipeline glue) |

**~159k lines of Rust across 19 crates**, 16 migrations, 5 worker binaries +
the CLI.

### Entry points

- **`voom-cli/src/main.rs`** — the `voom` binary. Routes all subcommands; even
  clap parse failures emit a JSON envelope. Exit codes `0/1/2`.
- **`voom-{ffmpeg,ffprobe,mkvtoolnix,verify-artifact}-worker/src/main.rs`** —
  worker binaries; bind loopback, read credentials from env, serve the protocol.
- **`voom-api/src/lib.rs`** — axum router constructed in-process; **no `main.rs`
  / server binary exists yet** (correctly documented as a later surface).

### Architectural health (overall)

The architecture is **genuinely strong for a pre-release system**: strict
one-way crate layering, an explicit "tickets route work, events record facts"
separation, a hard `connect()`-never-migrates invariant, deterministic plan
hashing with volatile-field stripping, a clean public error-code contract, and
an enforced sibling-test layout backed by a custom CI guard. The lint posture
(`forbid unsafe`, deny panic/unwrap/print, pedantic clippy) is stricter than
most production codebases. The dominant risks are **not** architectural — they
are (a) a handful of check-then-act sequences whose atomicity depends on
assumptions about SQLite write serialization that should be proven by test, (b)
input-robustness gaps (parser depth, numeric literals, worker path containment),
and (c) the size of `voom-control-plane` (several 1,000–2,100-line orchestration
modules) becoming a comprehension and test-coverage liability.

---

## 2. Findings by severity

Severity key: **Critical** = data loss / invariant violation likely in normal
operation · **High** = correctness or contract bug reachable under realistic
conditions · **Medium** = robustness / latent-bug / scale risk · **Low** =
hygiene, docs, polish.

### CRITICAL

**None confirmed.** The two items the sub-audits initially tagged "critical"
both weaken under scrutiny:

- The **remote-acquire capacity recheck race**
  (`voom-control-plane/src/cases/execution/remote_execution.rs:494–621`). **[VERIFIED
  — original downgrade rationale was wrong.]** The recheck and the insert *are*
  in one transaction, but that transaction is opened with `pool.begin()`
  (`cases/mod.rs:27`), which issues a **deferred** `BEGIN`, not `BEGIN IMMEDIATE`.
  The write lock is therefore acquired lazily, at the first write statement —
  *after* the capacity recheck has already read. Two concurrent acquires can both
  read the same pre-lease count, both see capacity, and only then serialize on the
  write lock; the loser gets `SQLITE_BUSY` after `busy_timeout` but the winner has
  already inserted against a stale check. SQLite's single-writer rule prevents a
  *crash*, **not** the stale-read interleaving. The race is reachable. **Raised
  back toward High**; see M6 for the fix (a single guarded `INSERT … WHERE
  (SELECT COUNT(*) …) < limit`, chosen so atomicity is structural rather than
  dependent on transaction isolation).
- The **verify-artifact-worker path traversal**
  (`voom-verify-artifact-worker/src/handler.rs` → `observe.rs`) is real as a
  *defense-in-depth inconsistency* (the ffmpeg worker enforces staging-root
  containment via `validate_staging_path`; verify does not — confirmed at
  `voom-ffmpeg-worker/src/handler.rs:673`). But the `request.path` is
  constructed by the **control plane** (`voom-control-plane/src/artifact/verify.rs:141`),
  which is the trust root, not by an external caller. Reclassified **Medium**.

### HIGH

| ID | Title | Location | Verified? |
|---|---|---|---|
| H1 | Policy parser has no nesting-depth limit → stack-exhaustion on pathological input | recursion at `voom-policy/src/syntax/parser.rs:231–238,261` (not the `nested` counter at :172) | Verified w/ correction |
| H2 | Numeric literals validated only as "all ASCII digits" — no range/length bound; silent `None` at `parse::<u64>()` | `voom-policy/src/compile/lower/conditions.rs:93`; sink `voom-plan/src/planner.rs:928` | Verified |
| H3 | No request/connection timeout on the worker-protocol **HTTP client**; a hung worker can stall a lease indefinitely | `voom-worker-protocol/src/http/client.rs` (no `timeout`/`Duration` present) | Verified (absence) |
| H4 | `voom-api` execution routes map only 4 error codes; `DB_UNREACHABLE` etc. collapse to HTTP 500 instead of 503 | `voom-api/src/lib.rs:~219–230` | Verified (fix corrected — do not reuse health mapper) |
| H5 | Pre-commit hooks run a strict subset of `just ci` — code can pass hooks yet fail CI | `.pre-commit-config.yaml` vs `justfile` `ci:` | Verified (delta corrected; `audit` is in both) |

**H1 — Parser nesting depth. [VERIFIED w/ correction.]** Stack exhaustion on
deeply nested policy text is real, but the *mechanism* differs from the original
description. The `nested: usize` counter at parser.rs:172 is part of an
**iterative** brace scanner — it does not drive recursion. The actual recursive
descent is `parse_statement` → `parse_statements_until` (parser.rs:251–264) →
`parse_statement`, taking one stack frame per block-body nesting level (recursive
calls at parser.rs:231–238, 261). No depth guard exists anywhere. *Direction:*
thread a depth counter through `parse_statements_until`/`parse_statement` (or add
it as a `Parser` field), error past a ceiling (~64) with a spanned diagnostic.

**H2 — Numeric literals. [VERIFIED.]** A literal is accepted if every byte is an
ASCII digit (conditions.rs:93); the text is stored verbatim as a
`CompiledValue::Number` string with no length/range bound. The downstream parse
is **`parse::<u64>().ok()`** at `voom-plan/src/planner.rs:928` (not `i64`), so an
over-long literal does not error — it silently returns `None` and the condition
silently never matches (planner.rs:845–850). A silent wrong-answer is worse than a
hard failure.
*Direction:* validate range/length at lower-time (or parse to `u64` immediately
and store the integer), emitting a clear diagnostic on overflow.

**H3 — Worker HTTP client timeout.** `http/client.rs` contains no `timeout` or
`Duration` usage. Leases have TTLs, but a worker that accepts a connection and
never responds parks the awaiting control-plane task until the OS gives up.
Note the *worker subprocess side is fine* — `run_ffmpeg_transcode` wraps the
child in `timeout(config.process_timeout, …)` (`voom-ffmpeg-worker/src/ffmpeg.rs:348`).
The gap is the control-plane→worker HTTP round-trip. *Direction:* wrap client
calls in `tokio::time::timeout` bounded to a fraction of the lease TTL. Loopback
makes this lower-probability, hence High-not-Critical.

**H4 — API error→status mapping. [VERIFIED w/ correction; line nums off by ~80.]**
`voom_route_error_response` (lib.rs ~219–230) maps `NotFound`, `Conflict`,
`ConfigInvalid`, `BadArgs` and sends everything else to 500, so a DB outage during
a lease op returns 500 ("your request was wrong") rather than 503 ("dependency
down"). The bug is real and `voom-api` has no `main.rs` yet, so fix before it
ships. **Correction to the original fix:** do *not* reuse `voom_error_response` —
that mapper (lib.rs ~136–217) is health-route-specific, with operator/DB-restore
hint text it would leak onto execution routes. *Direction:* add explicit arms in
`voom_route_error_response` mapping the `DbUnreachable`/`DbPartialSchema`/
`DbSchemaTooNew`/`DbDirtyMigration` family to 503, leaving genuine internal errors
in the 500 catch-all; add a DB-down route test.

**H5 — Hook/CI drift. [VERIFIED w/ correction.]** Pre-commit runs fmt-check,
clippy, test — **and `cargo-audit` (the original list omitted this)**. The actual
CI-only delta is `check-test-layout`, `check-paused-time-db`,
`check-paused-time-db-selftest`, `doc`, `deny` (not `audit`). Additionally, the
pre-commit clippy hook lacks `--all-features` that the `justfile` `lint` target
uses, so feature-gated code can pass hooks and fail CI clippy. *Direction:* add
the cheap guards to pre-commit and `--all-features` to the clippy hook, and/or
document "run `just ci` before push".

### MEDIUM

**Storage / SQLite**

- **M1 — `PRAGMA synchronous` not set explicitly** (`voom-store/src/pool.rs:32–47`).
  **[VERIFIED w/ nuance.]** WAL is enabled but `synchronous` is left at the SQLite
  default — which in WAL mode is `FULL` (safe, but fsync-heavy), *not* an unsafe
  value. So this is a perf/explicit-intent fix, not a durability bug: relying on an
  unstated default is a footgun for the "durable tickets" invariant. *Direction:*
  set `.synchronous(SqliteSynchronous::Normal)` explicitly (on-disk path only) and
  comment why.
- **M2 — Disk pool `max_connections(8)` against a single-writer engine, no
  `acquire_timeout`** (`voom-store/src/pool.rs:54–61`). Seven of eight connections
  serialize behind the writer; with no `acquire_timeout`, a stalled writer can
  block acquirers indefinitely (the 30s `busy_timeout` covers lock-wait, not
  pool-wait). *Direction:* add an `acquire_timeout`; reconsider whether 8 helps
  given write serialization. Note WAL *does* let readers proceed, so 8 is
  defensible for read concurrency — but the missing `acquire_timeout` is the real
  gap.
- **M3 — Issue dedupe NULL-key hole. [REFUTED.]** The migration `0008` partial
  unique index is `WHERE dedupe_key IS NOT NULL` as described, but the Rust API
  makes the hole unreachable: `PolicyIssueDraft.dedupe_key` is a non-optional
  `String` (issues.rs:9–15), bound as a non-NULL TEXT value (issues.rs:101), and
  the read-back `PolicyIssueRow.dedupe_key` is also `String`. No NULL key can reach
  the DB through the exposed API, so the duplicate-accumulation mechanism cannot
  occur. The column being nullable at the schema layer is harmless given the
  compile-time non-null type. **No action required.**
- **M4 — JSON payload columns carry no schema version** (`tickets.payload`,
  `commit_intents.target`, etc.). Rolling a binary that changed a payload struct
  risks silent field drop (serde default) on old rows. *Direction:* add a
  payload `schema_version` column or a documented forward/back-compat contract.
- **M5 — Heartbeat can move `expires_at` backwards. [VERIFIED — `[unverified]`
  tag removed.]** `heartbeat_in_tx` (leases.rs:269–272) runs `UPDATE leases SET
  last_heartbeat_at = ?, expires_at = ?, epoch = epoch + 1 WHERE id = ? AND state
  = 'held'`, binding `expires_at = now + ttl` unconditionally — no guard that the
  new deadline ≥ the old. A short-TTL heartbeat shortens the lease and a concurrent
  `expire_due_in_tx` can then expire it prematurely. Real correctness bug.
  *Direction:* add `AND expires_at <= ?` to the `WHERE` clause (binding the new
  deadline) so the update is monotonic.

**Control-plane orchestration** *(largest comprehension/test risk; several items
are [unverified] hypotheses from reading 1,000–2,100-line modules)*

- **M6 — Remote-acquire capacity check is two statements, not one guarded write**
  (`cases/execution/remote_execution.rs:494–621`). **[VERIFIED — promote toward
  High; see CRITICAL note.]** `[unverified]` tag removed. The recheck
  (`active_lease_count_for_worker_operation_in_tx`, ~601) and the lease INSERT
  (`acquire_lease_in_tx`, ~691) are in one transaction, but it is opened with a
  *deferred* `BEGIN` (`cases/mod.rs:27`), so the write lock is taken only at the
  INSERT — after the recheck has read. Two concurrent acquires can both read a
  stale pre-lease count; the race is reachable, not "likely safe." **Chosen fix:**
  a single guarded statement — `INSERT … SELECT … WHERE (SELECT COUNT(*) FROM
  leases WHERE worker_op = ? AND state='held') < ?` — so the check and the act are
  one atomic write, correct regardless of transaction isolation. (`BEGIN IMMEDIATE`
  would also close it but keeps atomicity incidental to config.)
- **M7 — Idempotency replay that fails to deserialize is not recorded as
  terminal** (`remote_execution.rs:307–311`, decode at `~2069`). **[VERIFIED —
  `[unverified]` tag removed.]** On a `Replay`, `replay_acquire` calls
  `serde_json::from_value`; on failure the error is captured, `commit_tx` commits
  (the replay path only read), and the error returns — the row stays
  `status='completed'` with unreadable JSON. Every retry with the same key hits the
  identical poison path forever; the slot can never succeed or be reused.
  *Direction:* on decode failure, mark the entry terminal (new poison status, or
  delete-in-tx) instead of committing the unreadable row and erroring.
- **M8 — Phase-loop snapshot staleness. [REFUTED.]** `finalize_phase` writes each
  updated file (with `file.snapshot = snapshot`, coordinator.rs:1719) back into the
  survivor set via `*files = survivors` (1639), and `recombine_survivors`
  (566–568) copies `entry.entering` — carrying the refreshed snapshots — into
  `self.files`, which the next phase plans against. Refreshed snapshots **do**
  propagate. (A narrow *passthrough*-file staleness variant — files skipped below
  `resume_ordinal` and mutated on disk between phases — is theoretically possible
  but depends on storage invariants outside the coordinator and is not what the
  finding claimed.) **No action required for the claim as written.**
- **M9 — Artifact commit target-collision check is not atomic with the link**
  (`artifact/fs.rs:85–98` check, `artifact/commit.rs:742` link). **[VERIFIED —
  lower impact than stated.]** `symlink_metadata` then `hard_link` is a genuine
  TOCTOU, but `hard_link` handles `AlreadyExists` → `VoomError::CommitFailure` →
  `transition_recovery`, so the failure mode is a *spuriously stuck
  `recovery_required` artifact*, not a silent data clobber. `recovery_required` is
  terminal with no in-process recovery entrypoint (confirmed). *Direction:* prefer
  an atomic `O_EXCL`/`link(2)` (which already errors on collision — the value is
  avoiding the redundant pre-check) and add an `attempt_commit_recovery`
  entrypoint.

**Worker protocol / workers** *(trust root is the control plane; these are
robustness/defense-in-depth)*

- **M10 — verify-artifact-worker lacks staging-root containment** that the
  ffmpeg worker enforces (`voom-verify-artifact-worker/src/handler.rs`,
  `observe.rs`). Inconsistent hardening; a control-plane bug could direct it to
  hash any path. *Direction:* mirror `validate_staging_path` /
  `open_regular_file_no_follow`.
- **M11 — NDJSON duplicate-frame handling recurses per dropped frame**
  (`voom-worker-protocol/src/wire/ndjson.rs:161–166`). **[VERIFIED w/ correction.]**
  `Box::pin(self.next_frame())` on every duplicate builds an async-recursion chain.
  Correction: depth is bounded by the **count of consecutive duplicate-seq frames**,
  not the 64 KiB per-frame *byte* limit (the byte limit caps each frame's size, not
  how many recursive calls stack). *Direction:* convert to a loop. Impact low on
  loopback.
- **M12 — Handshake endpoint is unauthenticated. [VERIFIED in part; body-cap claim
  REFUTED.]** `route_policy` for `/v1/handshake` returns `auth: false` and
  `version: false` — but both are **by design** (handshake negotiates the protocol
  version before auth context exists). The "exempt from the body-size cap" claim is
  **false**: `handle_request` calls `read_body` (enforcing `MAX_BODY_BYTES = 1 MiB`)
  *before* route dispatch, so the cap applies to handshake and every other route
  uniformly. **No body-cap action required.** The only residual item is whether
  handshake should carry any authentication at all, which the trust model makes a
  low-priority defense-in-depth consideration.
- **M13 — No per-connection read timeout on the hyper server** (slowloris on
  loopback). Low probability; *Direction:* wrap `serve_connection` in a timeout.
- **M14 — ffmpeg/ffprobe/mkvmerge filenames not rejected when leading `-`**
  (`voom-ffmpeg-worker/src/ffmpeg.rs:316,345`). Input after `-i` is consumed as a
  value (safe); the **output positional** (`command.arg(output)`) is the real
  exposure, but output paths *are* canonicalized under the staging root by
  `validate_staging_path`, which neutralizes most of it. Net: **Low-Medium**;
  add an explicit leading-`-` reject for clarity and to cover the input path.

**Domain / events**

- **M15 — Event enum has no `#[serde(other)]` fallback** (`voom-events/src/payload/mod.rs`).
  An older binary reading a newer DB's event kind fails to deserialize. Append-only
  + per-kind versioning makes this tolerable today, but it blocks mixed-version
  reads. *Direction:* add an `Unknown(serde_json::Value)` catch-all or document
  the upgrade-ordering requirement.

**Infra**

- **M16 — Release artifacts are unsigned and unchecksummed** (`.github/workflows/release.yml`).
  `softprops/action-gh-release` publishes `.tar.gz` with no SHA256 / provenance /
  attestation. *Direction:* generate checksums and add
  `actions/attest-build-provenance` before distributing binaries to users.
- **M17 — `chaos-e2e.yml` is `workflow_dispatch`-only** — can silently rot.
  *Direction:* add a scheduled run on `main` with failure notification.
- **M18 — Docs drift: `voom-artifact` described as "reserved, no logic"**
  (AGENTS.md / README) but `crates/voom-artifact/src/commit_pipeline.rs` now holds
  real commit-pipeline glue (confirmed). *Direction:* update both docs.

### LOW

- **L1** — `voom worker run-local` prints a bare `{"status":"ready",…}` line to
  stdout *before* the final envelope (`emit_ready_line` def at worker.rs:117, call
  at :156). **[VERIFIED w/ correction.]** `run-local` is a **long-running
  foreground supervisor**, so a readiness line + final shutdown envelope is a
  defensible streaming contract. Correction: the readiness line's *shape* **is**
  unit-tested (worker_test.rs:40–71) — the gap is only an end-to-end test of the
  two-line stdout protocol and documentation of the contract. *Direction:* add the
  integration test and document the two-line contract. (Downgraded from Critical.)
- **L2** — Error mapping wraps `sqlx::Error` into `VoomError::Database(String)`
  (error.rs:216), dropping the structured source chain (**~641 call sites**, not
  ~420). Add a `#[source]`-bearing variant for triage. **[VERIFIED; count
  corrected.]**
- **L3** — Scheduler `ScoreReasonCode::priority()` ordering is correct but
  implicit (`voom-scheduler/src/lib.rs:113`); add a doc comment naming the tiers.
- **L4** — ID newtypes accept `0` and any `u64` with no boundary validation
  (`voom-core/src/taxonomy/ids.rs`); fine given DB-generated IDs, worth a note.
- **L5** — Migrations are up-only (no down path); document the manual rollback
  procedure for operators.
- **L6** — Migrations `0012`/`0013` do `COMMIT; PRAGMA …; BEGIN;` inside the
  migrator wrapper; safe (PRAGMAs are session-scoped) but non-obvious — add a
  comment explaining why the explicit commit is required.
- **L7** — `voom-fakes` is a workspace member but missing from
  `[workspace.dependencies]`; consumers reference it by path. Add it for
  consistency.
- **L8** — Dependabot groups minor/patch only; majors ungrouped (noise, not
  correctness).
- **L9** — `voom-api` clones `AppState` (incl. `ControlPlane`) per request; the
  clone is shallow (Arc/pool handles) so this is micro-efficiency, not a bug.
- **L10** — Several control-plane modules exceed 1,000 lines
  (`remote_execution.rs` 2,143; `coordinator.rs` 1,896; `executor.rs` 1,438;
  `commit.rs` 1,020 — all confirmed by `wc -l`). **Correction:** the listed
  `planner.rs` (claimed 1,138) is **not a control-plane module** — it lives in
  `voom-plan/src/planner.rs` and is actually 1,166 lines; the largest
  control-plane planning-adjacent file is `cases/policy/compliance.rs` (1,087).
  Not defects, but the primary drag on auditability and test coverage. Flagged for
  the Polish phase.

### What the sub-audits got wrong (corrections kept honest)

- ffmpeg subprocess "has no timeout" → **false**; it wraps the child in
  `timeout(config.process_timeout, …)`.
- Remote-acquire / lease-expiry "Critical race" → **overstated**; SQLite write
  serialization makes the described interleaving unlikely. Kept as Medium
  `[unverified]`.
- verify-worker path traversal "Critical (read /etc/passwd)" → **overstated**;
  the path comes from the trusted control plane, not an external attacker.
- `run-local` "Critical envelope violation" → **overstated**; it's a foreground
  supervisor with a streaming contract, downgraded to a docs/test gap (L1).
- Plan determinism / `generated_at` leak / HashSet ordering → **non-issues**;
  the code correctly strips volatile fields, canonicalizes JSON key order, and
  uses HashSet only for dedup, not output ordering. No determinism defect found.

### What the verification pass corrected (this audit's own errors)

- **M3 (NULL `dedupe_key`) → REFUTED.** The Rust type is `String`, not
  `Option<String>`; no NULL reaches the DB. Mechanism impossible.
- **M8 (snapshot staleness) → REFUTED.** Refreshed snapshots *do* propagate into
  the survivor set (`coordinator.rs:1639` → `recombine_survivors:566–568`).
- **M6 downgrade rationale → WRONG.** The acquire transaction uses a deferred
  `BEGIN`, so SQLite write serialization does not eliminate the race. M6 raised
  back toward High; fix is a single guarded `INSERT … WHERE … < limit`.
- **M12 "exempt from body cap" → FALSE.** `MAX_BODY_BYTES` is enforced on all
  routes via `read_body` before dispatch. Only the (by-design) unauthenticated
  handshake remains.
- **L10 `planner.rs` (1,138 lines) → miscategorized.** The file is in `voom-plan`
  (1,166 lines), not a `voom-control-plane` module; the other four file sizes are
  correct.
- Smaller corrections: H1 recursion is via `parse_statements_until`, not the
  `nested` counter; H2 sink is `parse::<u64>()` (silent `None`), not `i64`; H4
  must not reuse the health-route mapper; H5 `cargo-audit` already runs in
  pre-commit; M1 default is `FULL` (safe, slow), not unsafe; L1 readiness line
  *is* unit-tested; L2 is ~641 call sites, not ~420.

---

## 3. Prioritized milestone plan

Four phases, ordered so that each phase makes the next one safe to do. **No
fixes are applied in this report** — this is the proposed sequence.

### Phase 1 — Safety net *(build confidence before touching logic)*

Goal: make the risky areas *observable and testable* so later fixes can be
verified, and close the cheapest correctness/robustness gaps.

1. **Concurrency test harness for tickets/leases** — a test that hammers
   `acquire`/`heartbeat`/`expire` from many tasks against a real on-disk WAL DB.
   This is the prerequisite for confirming or dismissing M5, M6, M9 rather than
   guessing. (Respect the AGENTS.md rule: real time + `ManualClock`, never
   `tokio::time::pause` with a `SqlitePool`.)
2. **Multi-phase workflow test** covering a mid-loop dispatch failure (strand
   check). (Note: M8 snapshot-propagation is already verified correct, so this is
   now a regression guard, not a bug-confirmation.)
3. **Add `acquire_timeout` to the pool (M2)** and **explicit `PRAGMA synchronous
   = NORMAL` (M1)** — two-line, high-value durability/liveness fixes.
4. **Align hooks with CI (H5)** — add the cheap guards to pre-commit and/or
   document `just ci` before push.
5. **Fix docs drift (M18)** and reconcile README/AGENTS.md `voom-artifact`
   language.

### Phase 2 — Critical fixes *(correctness the system depends on)*

Goal: eliminate the confirmed correctness/contract bugs and the
once-verified-now-prioritized races.

1. **H3 — worker HTTP client timeout.** Bound every control-plane→worker call.
2. **H4 — `voom-api` error→status mapping.** Reuse the exhaustive mapper; add a
   DB-down route test. Do this before any server binary ships.
3. **M6 — make remote capacity enforcement structural** via a single guarded
   `INSERT … WHERE (SELECT COUNT(*) …) < limit` (verified as a reachable race
   under the deferred-`BEGIN` transaction, *not* the Medium "likely safe" the
   sub-audit assumed), once Phase-1 tests have characterized the current behavior.
4. **M5 — monotonic `expires_at` guard** on heartbeat (`AND expires_at <= ?`).
5. **M7 — terminal idempotency on replay decode failure.**
6. ~~M3 — NULL-`dedupe_key` hole~~ — **dropped: refuted** (the Rust type is
   non-nullable `String`; no NULL path exists).

### Phase 3 — High leverage *(robustness & input hardening)*

Goal: make the agent-facing and worker-facing surfaces resilient to bad input.

1. **H1 / H2 — parser depth limit + numeric-literal validation** (both
   verified; both cheap; both protect the agent-facing CLI).
2. **M10 — verify-artifact staging-root containment** (parity with ffmpeg
   worker).
3. **M11 — NDJSON recursion → loop**; **M13 — server read timeout** (M12 body-cap
   is already enforced on all routes — refuted); **M14 — leading-`-` filename
   reject**.
4. **M9 — atomic commit-target link + a recovery entrypoint.**
5. **M4 / M15 — payload schema versioning + event `#[serde(other)]`** (decide
   the mixed-version-read contract deliberately).
6. **M16 — release artifact checksums + provenance.**

### Phase 4 — Polish *(maintainability, hygiene, docs)*

Goal: reduce the long-term audit/maintenance drag.

1. **L10 — decompose the 1,000–2,100-line control-plane modules** along the
   seams the sub-audit mapped (coordinator vs executor vs promotion; remote
   execution acquire/replay/recheck). This is the single biggest lever on future
   auditability and test coverage.
2. **L2 — structured `sqlx::Error` source** in `VoomError`.
3. **L1 — document/test the `run-local` streaming contract.**
4. **L5 / L6 — migration rollback runbook + migration PRAGMA comments.**
5. **L3 / L4 / L7 / L8 / M17 — scheduler doc comment, ID-boundary note,
   `voom-fakes` workspace dep, Dependabot grouping, scheduled chaos-e2e.**

---

## 4. One-paragraph verdict

VOOM is a well-disciplined, architecturally sound pre-release codebase whose
guardrails (lints, layering, test-layout enforcement, paused-time DB guard,
deterministic hashing, public error contract) are notably stronger than typical.
There are **no confirmed critical defects**, but the verification pass did
confirm three load-bearing correctness bugs the sub-audits left as hypotheses:
**M6** (a *reachable* capacity race — the deferred-`BEGIN` transaction does not
serialize the recheck against the insert, contrary to the original downgrade),
**M5** (heartbeat can shorten a lease's deadline), and **M7** (a replay decode
failure poisons an idempotency key permanently). The most important real work is
(1) fixing M5/M6/M7 behind a concurrency test harness, (2) bounding the worker
HTTP client and completing the `voom-api` error mapping (mapping the DB-down
family to 503, *without* reusing the health-route mapper) before that surface
ships, and (3) routine input hardening on the policy parser and worker path
handling. Two sub-audit findings were refuted outright (M3, M8) and one miscategorized
`voom-plan/planner.rs` as a control-plane module. The largest *latent* risk is sheer module size
in `voom-control-plane`, which the Polish phase should address so future audits
and tests can keep pace. Recommended order is the
four-phase plan above: build the test net first, then fix correctness, then
harden inputs, then decompose and polish.
