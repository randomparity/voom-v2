# Cancellation-safe `BEGIN IMMEDIATE` — implementation plan

Issue #592 · spec: `docs/workflow/specs/2026-08-27-cancelled-begin-immediate-lock-leak-design.md` · ADR 0087
Governing rules: ADR 0083 (read-then-write ⇒ `BEGIN IMMEDIATE`), ADR 0086 (four named openers).
Supersedes one sentence of ADR 0068 (the `conn.begin_with` mechanism, not its substance).
Deferral in this diff: `docs/debt/0005-connection-level-custom-begins-are-unguarded.md`.

Guardrails: `just ci` (fmt-check, lint, check-test-layout, check-paused-time-db,
check-control-plane-sql-boundary, check-check-constraint-bypass,
check-payload-deny-unknown, check-transaction-openers, check-adr-index,
select-ffmpeg-asset-selftest, run-constrained-selftest, test, doc, deny, audit),
each with its `-selftest` sibling. The ADR index gate is coupled and
CI-hard-gated, so ADR 0087's row lands in this PR.

Conventions: AGENTS.md — sibling `*_test.rs` via `#[path]`; integration tests in
`crates/*/tests/`; never pair `tokio::time::pause` with a real `SqlitePool`;
tests run on the pinned `.test-tmp/` root (ADR 0079).

## Global constraints

- **Delete the scratch probe first.** `crates/voom-store/tests/zz_probe_cancel_begin.rs`
  is untracked design scaffolding. It is removed before T1, not committed and not
  renamed — `cancelled_begin_releases_write_lock.rs` is written fresh from the
  spec, because the probe's fixture is one of the two the spec now records as
  producing *different* answers.
- **Lints bind the test file too.** `just lint` is
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and the
  workspace denies `unwrap_used`, `panic`, `print_stdout`, `print_stderr`, and
  `allow_attributes`. So the test opens with `#![expect(clippy::unwrap_used, …)]`
  — plus `clippy::expect_used` and `clippy::print_stderr` as it uses them — each
  with a `reason =`, matching every other integration test in
  `crates/voom-store/tests/`. `expect_used` is only `warn`, but `-D warnings`
  makes that fatal. `#[allow]` is denied outright; it must be `#[expect]`.
- **Callers are untouched.** ~50 files call the two openers; the change is inside
  them. That is the ADR 0086 property being cashed in, and no call site moves.
- **No production dependency changes.** `tokio` gains the `rt` *feature*;
  `tracing-subscriber` joins `voom-store` as a **dev**-dependency only.
- **The spec's test design is not settled.** Its review cycle exited at budget
  with the Testing section rewritten three times, each rewrite producing new
  defects in the next pass. Treat T2–T4 as the spec's *intent*, verify each arm
  bites (below), and expect the branch review at quest step 6 to be where the
  test design is actually settled.

## T0 — Remove the scratch probe

Files: `crates/voom-store/tests/zz_probe_cancel_begin.rs` (delete).

Verify: `git status --short --untracked-files=all` is clean.

## T1 — Manifest: `tokio` `rt`, `tracing-subscriber` dev-dep

Files: `crates/voom-store/Cargo.toml`.

`tokio = { workspace = true, features = ["sync", "time", "rt"] }` — `tokio::spawn`
needs `rt`. `tracing-subscriber = { workspace = true }` under `[dev-dependencies]`,
at the version the workspace already pins for `voom-api`, `voom-control-plane`
and `voom-cli`. `tracing` is already a normal dependency; `oneshot` is already
covered by the `sync` feature.

Verify: `cargo check -p voom-store --all-features`; `just deny`; `just audit`.
No new entry appears in `Cargo.lock` beyond `tracing-subscriber`'s existing
workspace graph (it is already built for three other crates).

## T2 — TDD red: the regression sweep and its positive control

Files: `crates/voom-store/tests/cancelled_begin_releases_write_lock.rs` (new).

Written against the **unfixed** openers, and it must fail. Both arms per the
spec's Testing section:

- Poll helper: polls a future for exactly *N* wakeup-driven polls, then drops it.
  **No self-wake** — a `cx.waker().wake_by_ref()` on `Pending` busy-spins, the
  sqlx worker never gets scheduled, and the sweep measures nothing. It returns
  whether it dropped a still-pending future.
- Fixture, pinned and load-bearing: a fresh `TempDatabase`, a fresh
  `voom_store::connect` pool, and a fresh observer **per *N*, per arm**. One
  completed transaction through the pool as warm-up before the cancellation —
  the measured N table only reproduces in the warm shape. No arm shares a pool
  or a file with another.
- Observer: a raw `SqliteConnectOptions` connection with `busy_timeout(0)` —
  **not** `voom_store::connect`, whose 30s would turn "blocked" into a
  wall-clock expiry. Retries on `SQLITE_BUSY` up to 5s. 100ms settle after the
  cancellation before the first attempt.
- Fixed arm: sweep *N* in 1..=8, **5 repeats**, no blocked observation in any.
- Control arm: same sweep through a bare `pool.begin_with("BEGIN IMMEDIATE")`
  written in the test, **5 repeats**, at least one blocked observation across
  them. Gated on `available_parallelism() >= 4`; the fixed arm is **not** gated.
  Skip notice via `eprintln!` under the file-level `expect`.

Verify (this is the red): `cargo test -p voom-store --test
cancelled_begin_releases_write_lock --all-features` — the fixed arm fails, the
control arm passes. Record which *N* blocked, for the PR body.

## T3 — TDD green: `begin_detached`

Files: `crates/voom-store/src/tx.rs`.

Add the private `begin_detached(pool, statement, context)` from the spec's Design
section, and route `begin_read_then_write` (`:40`) and `begin_serialized_read`
(`:105`) through it. `begin_write_first` and `begin_read_only` are **unchanged** —
a deferred `BEGIN` takes no write lock and the worker's rendezvous ack already
rolls back a cancelled open.

Needs `use tracing::Instrument;` for `.instrument(tracing::Span::current())`,
which is what keeps the detached open inside the caller's span — `tokio::spawn`
does not inherit one. Both `send`-failure arms log; the `Err` arm attributes an
`acquire_timeout` that fires for a caller answered 45s earlier.

The module doc comment gains the cancellation-safety property, since the file's
existing doc explains *why these four functions exist* and this adds a second
reason.

Verify: T2 goes green, both arms. `just lint` clean (the `pedantic` group over a
new async fn returning `Result` is the likely source of noise). Reverting T3
alone must put T2's fixed arm back to red.

## T4 — The orphan arm

Files: `crates/voom-store/tests/cancelled_begin_releases_write_lock.rs`.

Written after T3 because it asserts on a `warn` that only exists post-fix. Five
steps per the spec: holder takes `BEGIN IMMEDIATE`; the caller runs
`begin_read_then_write` under a short `tokio::time::timeout` that is *guaranteed*
to fire while the holder owns the lock; the holder rolls back; **wait for the
orphan `warn`**; only then probe the lock with a `busy_timeout(0)` connection.

The subscriber must be **global** (`tracing_subscriber::registry().with(layer)`
behind a `OnceLock`), not `tracing::subscriber::with_default`. `with_default` is
thread-local and the `warn` fires on a spawned task on another worker thread, so
a scoped subscriber captures nothing and the arm silently passes. The capture
layer records events into a shared buffer; the arm filters on the unique
`context` string it passed in, so tests in the same binary cannot cross-talk.

Do not skip the wait: measured 20/20, the observer wins the lock 0.36–0.67ms
after the holder's rollback while the detached opener is still parked in SQLite's
busy handler sleep, with the `send` error branch not yet executed. Probe-only,
this arm passes against a `begin_detached` that rolls nothing back.

Verify it bites — the arm is not red pre-fix, so a revert does not test it.
Controlled fault: in the spawned task, wrap the unsent `Transaction` in
`std::mem::ManuallyDrop` so the `ROLLBACK` is never queued — **not**
`std::mem::forget`, which clippy denies (`mem_forget = "deny"`), so the fault
itself would fail `just lint` before it could be observed. The arm must go red on
the lock probe. Revert the fault.

## T5 — `init.rs` joins the opener vocabulary

Files: `crates/voom-store/src/init.rs`.

`run_migrations_on` at `:50-57` does `pool.acquire()` then
`conn.begin_with("BEGIN IMMEDIATE")` — the same two-step path with the same
window, and invisible to `check-transaction-openers.sh` because that rule
constrains the receiver to `(?i)pool`. It becomes
`begin_read_then_write(pool, "acquire migration write lock")`.

Three consequences: the `acquire connection for migration` error context
disappears with the explicit acquire; both `drop(conn)` lines (`:69`, `:77`) go,
because a pool-level `Transaction<'static>` owns its connection and returns it on
commit or drop, which is the only reason those lines existed; and
`use sqlx::Connection;` at `:1` becomes unused — it is there solely for
`conn.begin_with` — so it must be removed or `-D warnings` fails.
`MIGRATOR.run_direct(&mut *tx)` and the ADR 0068 doc comment's substance are
unchanged.

Verify: `cargo test -p voom-store --test init --all-features`; the existing
init/migration coverage is what exercises this, and no new test is written for it
because the behaviour is unchanged and only the shape moved.

## T6 — Guardrails and the record

Files: none beyond what T1–T5 touched; `docs/` already carries ADR 0087, its
index row, the spec, and `docs/debt/0005`.

- `./scripts/check-transaction-openers.sh crates` still exits 0, and
  `rg -n 'begin_with' crates --type rust` now finds custom-statement opens only
  in `crates/voom-store/src/tx.rs` and in test files — the non-regression
  boundary `docs/debt/0005` states.
- `just check-transaction-openers-selftest` passes (the rule is unchanged; this
  confirms the change did not weaken it).
- `just ci` green.

## Acceptance run — completion criterion 5

Separate from the guardrails and reported honestly whether or not it completes.
Pre-fix control first and **gating**: the unfixed opener under
`./scripts/run-constrained.sh --load 1 --write-bps 40M -- cargo llvm-cov
--no-report -p voom-node-agent --test lifecycle --all-features -- --test-threads=1`,
stopping at the first reproduction or at 90 runs. If 90 unfixed runs do not
reproduce, criterion 5 is reported **not discharged** — not as a green sweep.
Then the same invocation post-fix, ≥90 runs. Report both counts and the pre-fix
reproduction index; the (29/30)^90 = 0.047 figure is conditional on #592's rate
transferring to this host, and the index is the host-local evidence for that.

Up to ~180 instrumented, throttled, serialized lifecycle runs. If it is not run
to completion, say so and say which criteria that leaves undischarged.

## Rollback

`git revert` the T3 and T5 commits. `begin_detached` is additive and the two
openers keep their signatures, so reverting restores the prior behaviour without
touching any of the ~50 call sites. The test file and the manifest change are
inert without it — the fixed arm returns to red, which is the correct signal.
