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
  — plus `clippy::expect_used` if it uses `.expect` — each with a `reason =`,
  matching every other integration test in `crates/voom-store/tests/`.
  `expect_used` is only `warn`, but `-D warnings` makes that fatal.
  `#[allow]` is denied outright; it must be `#[expect]`.
- **The `#![expect]` list must contain only lints the file provably trips.**
  `[workspace.lints.rust] warnings = "deny"` makes `unfulfilled_lint_expectation`
  fatal, so an over-broad expectation list fails `just lint` by itself — in the
  same file the list was added to make lint pass. Add a member only after seeing
  the lint fire.
- **No skip notice.** The control arm's parallelism gate is a silent early
  return. `libtest` prints a passing test's output only under
  `--nocapture`/`--show-output`, which neither `just ci` nor the coverage job
  passes, so an `eprintln!` would be invisible to every CI reader — and would
  cost a `clippy::print_stderr` expectation that has to stay in sync with
  whether the branch is taken. No tracked test in `crates/voom-store/tests/`
  expects `print_stderr` today.
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

Verify with **two** commands, because they prove different things and only one of
them can catch a wrong `rt`:

- `cargo check -p voom-store --all-features` — **lib only, and this is the
  detector.** An earlier draft of this plan said the opposite. `--all-targets` is
  precisely the invocation that *cannot* see a missing `rt`: voom-store's
  dev-dependency already carries `tokio = { features = ["rt-multi-thread",
  "macros"] }` (`Cargo.toml:42`), `rt-multi-thread` implies `rt`, and any build
  that compiles test targets unifies dev-dependency features into the lib
  compile. Measured on a probe crate with this exact feature shape and the pinned
  toolchain: bare `cargo check` exits 101 with `E0425 cannot find function
  spawn`; `cargo check --all-targets` exits 0; `cargo build --release` exits 101;
  `cargo doc` exits 0 because it does not type-check bodies. Every `just ci` step
  that compiles voom-store passes `--all-targets` or is `cargo doc`, so **no
  guardrail in this repository detects an omitted `rt`** — this one command is
  it. Re-run it at the end of T3, where `tokio::spawn` first appears; at T1
  nothing uses the feature yet, so T1's run proves only that the manifest parses.
- `cargo check -p voom-store --all-features --all-targets` — proves the
  `tracing-subscriber` dev-dependency resolves and compiles. That much it does do.

Then `just deny`, `just audit`.

`Cargo.lock` gains exactly one `"tracing-subscriber"` line in the `voom-store`
package's `dependencies` array and **no new `[[package]]` block** — the lock
format merges normal and dev dependencies into one array (`voom-store` already
lists `tempfile`, `voom-test-support` and `voom-control-plane` there), and
`tracing-subscriber` 0.3.23 is already in the graph for three other crates.

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
  wall-clock expiry. 100ms settle after the cancellation before the first
  attempt, then retries on `SQLITE_BUSY` up to **5s in the fixed arm** and
  **1s in the control arm**.
- **Budget the wall clock, because this test lands in the job #592 is about.**
  At a leaking *N* the observer necessarily burns its whole ceiling — the lock is
  held until the pool drops — and the warm fixture leaks at *N* = 3 in 40 of 40
  sweeps. A uniform 5s ceiling would cost ≥25s of guaranteed sleeping in the
  control arm alone; 1s costs ≥5s. Add 100ms × 8 *N* × 5 repeats × 2 arms = 8s of
  settle and the orphan arm's bounded wait: roughly **15s** expected, paid twice
  in CI — by `just test` and by the serialized instrumented `coverage` job whose
  duration is this issue's subject. The shorter control ceiling costs nothing:
  the spec establishes that "the bound is not what discriminates, the repeated
  `SQLITE_BUSY` is". Measure the real figure on the first green run and report it
  with T2's result; if it is materially above 15s, cut the repeats.
- Fixed arm: sweep *N* in 1..=8, **5 repeats**, no blocked observation in any.
- Control arm: same sweep through a bare `pool.begin_with("BEGIN IMMEDIATE")`
  written in the test, **5 repeats**, at least one blocked observation across
  them. Gated on `available_parallelism() >= 4` as a **silent early return**; the
  fixed arm is **not** gated.

Verify (this is the red): `cargo test -p voom-store --test
cancelled_begin_releases_write_lock --all-features` — the fixed arm fails, the
control arm passes. Record which *N* blocked, for the PR body.

Also run `just lint` at the end of T2, not only at T6: it is the only check that
catches an over-broad `#![expect]` list, and deferring every lint signal to T6
finds it three tasks after the file was written.

## T2b — The pre-fix acceptance control (gating, and it runs **here**)

Files: none. This task exists for its position in the order.

The pre-fix arm of the acceptance run must execute against the *unfixed* openers,
and this is the last point at which they exist: T3 detaches them. Running it here
costs nothing extra — `crates/voom-node-agent` and the openers are both untouched
at T2 — whereas running it after T6 needs a revert, a `git stash`, or
`git worktree add ../voom-592-prefix <merge-base>`, and each of those invalidates
the `llvm-cov` instrumented build, adding two full instrumented rebuilds of
`voom-node-agent` and its graph to a protocol already priced at ~180 runs.

Run the loop from *Acceptance run* below, unfixed, into `.tmp/accept-prefix/`.
Stop at the first log matching the predicate, or at 90 runs. Record the
**reproduction index** — the run number that first reproduced — because that
index is the host-local evidence the whole post-fix sweep's confidence rests on.
If 90 runs do not reproduce, criterion 5 is reported **not discharged**, and that
verdict is reached here rather than discovered at the end.

## T3 — TDD green: `begin_detached`

Files: `crates/voom-store/src/tx.rs`.

Add the private `begin_detached(pool, statement, context)` from the spec's Design
section, and route `begin_read_then_write` (`:40`) and `begin_serialized_read`
(`:105`) through it. `begin_write_first` and `begin_read_only` are **unchanged** —
a deferred `BEGIN` takes no write lock and the worker's rendezvous ack already
rolls back a cancelled open.

Needs two imports: `use tokio::sync::oneshot;` for the channel, and
`use tracing::Instrument;` for `.instrument(tracing::Span::current())`, which is
what keeps the detached open inside the caller's span — `tokio::spawn` does not
inherit one. Both `send`-failure arms log; the `Err` arm attributes an
`acquire_timeout` that fires for a caller answered 45s earlier.

The module doc comment gains the cancellation-safety property, since the file's
existing doc explains *why these four functions exist* and this adds a second
reason.

Verify: T2 goes green, both arms. **`cargo check -p voom-store --all-features`
(lib only)** — this is where the `rt` feature is first exercised and the only
place in the repository a missing one is detectable; see T1. `just lint` clean
(the `pedantic` group over a new async fn returning `Result` is the likely source
of noise). Reverting T3 alone must put T2's fixed arm back to red.

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

Verify it bites. The arm **is** red pre-fix — but at **step 4**, not step 5: the
unfixed opener is an inline `pool.begin_with`, there is no spawned task and no
`warn`, so the arm burns its 5s ceiling waiting for an event that never arrives.
That is a real red, and it is the wrong one: a reverting reader who sees red and
concludes "the arm bites" will never have exercised the lock probe, which is the
assertion the arm exists for. So the revert does not substitute for the fault
below — it only proves step 4 is wired.

Controlled fault, which isolates step 5: in the spawned task, wrap the unsent
`Transaction` in
`std::mem::ManuallyDrop` so the `ROLLBACK` is never queued — **not**
`std::mem::forget`, which clippy denies (`mem_forget = "deny"`), so the fault
itself would fail `just lint` before it could be observed. With the fault in
place step 4 still passes — the `warn` fires — and the arm must go red on **the
lock probe**. Revert the fault.

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

It also gains `use crate::tx::begin_read_then_write;`.

Verify: `cargo test -p voom-store --all-features init` — **both** targets.
`--test init` alone runs only `tests/init.rs` (4 tests) and misses the coverage
this task actually rests on, which lives in the lib target at
`crates/voom-store/src/init_test.rs`. Three tests there must stay green, and each
encodes an assumption about `run_migrations_on`'s *internal* acquire that T3
moves onto a spawned task:

- `busy_timeout_exhaustion_surfaces_database_error` (`:297`) — its own comment
  calls the setup load-bearing: `conn2` is returned to the idle queue so
  `run_migrations_on`'s internal `pool.acquire()` reuses that specific
  `busy_timeout = 0` connection. Post-T3 that acquire happens inside
  `begin_detached`'s spawned task against the same pool, so it should still draw
  the same idle connection — but this is the test that says otherwise if it does
  not.
- `locked_migration_true_race_reports_zero_applied` (`:268`) — the held-lock race
  ADR 0068 exists for.
- `single_shot_replacement_has_no_polling` (`:214`) — a <25ms wall-clock bound on
  the rollback-then-probe path, which now carries a task spawn.

No new test is written for `init.rs` itself: the behaviour is unchanged and only
the shape moved.

## T6 — Guardrails and the record

Files: none beyond what T1–T5 touched; `docs/` already carries ADR 0087, its
index row, the spec, and `docs/debt/0005`.

- `./scripts/check-transaction-openers.sh crates` still exits 0, and the
  non-regression boundary is this determinate predicate:

  ```
  rg -n 'begin_with' crates --type rust \
    -g '!**/tests/**' -g '!**/*_test.rs' -g '!crates/voom-test-support/**'
  ```

  returns only `crates/voom-store/src/tx.rs`. The earlier wording — "only tx.rs
  and test files" — could not distinguish success from failure: run on the tree
  today it also returns `crates/voom-test-support/src/commit_node.rs:89,110` and
  `staging_seed.rs:59`, which are `src/` files in a support crate, exempt from
  `check-transaction-openers.sh` by its
  `grep -Ev "/(voom-test-support|voom-fakes|…)/"` filter rather than by its
  test-file filter, and untouched by T5. `docs/debt/0005` **already carries this
  exact predicate** under its Non-regression boundary section, corrected when
  this plan was reviewed; T6 re-runs it and confirms the result is `tx.rs` alone.
  Leave that record unmodified — T6 touches no files.
- `just check-transaction-openers-selftest` passes (the rule is unchanged; this
  confirms the change did not weaken it).
- `just ci` green.

## Acceptance run — completion criterion 5

Separate from the guardrails and reported honestly whether or not it completes.

**The reproduction predicate is one specific `HANG_GUARD` expiry**, not a
non-zero exit status and not any guard firing. `HANG_GUARD` is 30s
(`crates/voom-node-agent/tests/lifecycle.rs:47`) and bounds **seven** waits in
that file (`:100, :163, :439, :460, :508, :531, :553`), each with its own message
— the file header at `:5-9` records that as deliberate, because "a bare
`Elapsed(())` panic reports only a line number, which is how #446 was filed
against the wrong wait". The expiry #592 reports surfaces through
`wait_for_graceful_shutdown` (`:452-491`) and panics at `:110`. So the literal
predicate is:

```
grep -q 'second agent graceful-shutdown lifecycle did not complete' "$log"
```

Verify that string against one real expiry before starting the sweep. **A
different `HANG_GUARD` site firing is a different failure, not a reproduction**,
and a non-zero exit is not evidence either — any unrelated failure in the
lifecycle binary also exits non-zero.

**The loop goes outside `run-constrained.sh`.** It wraps one command in its own
cgroup v2 scope with the `--write-bps` cap applied to that scope, so
`for i in $(seq 90); do ./scripts/run-constrained.sh … ; done` gives each run an
identical, un-depleted write budget, while
`run-constrained.sh … -- bash -c 'for i in …'` gives 90 runs one shared budget.
#592 records the throttle as *required* to reproduce, so the two placements are
not interchangeable and the outer one is the one this protocol means:

```
# $ARM is accept-prefix (T2b) or accept-postfix; each arm gets its own directory
# so the second cannot overwrite the first arm's evidence. `.tmp*/` is already
# gitignored; the repo root is not, and `*.log` is not ignored anywhere.
mkdir -p .tmp/$ARM
for i in $(seq 90); do
  log=.tmp/$ARM/run-$i.log
  ./scripts/run-constrained.sh --load 1 --write-bps 40M -- \
    cargo llvm-cov --no-report -p voom-node-agent --test lifecycle \
    --all-features -- --test-threads=1 >"$log" 2>&1
  if grep -q 'second agent graceful-shutdown lifecycle did not complete' "$log"
  then echo "reproduced at run $i"; break; fi
done
```

The `break` is what makes T2b's stopping rule executable; without it the pre-fix
arm runs all 90 iterations regardless. The post-fix arm runs the same loop with
`$ARM=accept-postfix` and **no** break — it must complete ≥90 runs — and any
match is a failure, not a stop.

Report both counts and the pre-fix reproduction index. The (29/30)^90 = 0.047
figure is conditional on #592's rate transferring to this host, and that index is
the host-local evidence for it.

Up to ~180 instrumented, throttled, serialized lifecycle runs. If it is not run
to completion, say so and say which criteria that leaves undischarged.

## Rollback

`git revert` the T3 and T5 commits. `begin_detached` is additive and the two
openers keep their signatures, so reverting restores the prior behaviour without
touching any of the ~50 call sites. The test file and the manifest change are
inert without it, and **both** test arms go red: the fixed arm on a leaked lock,
which is the correct signal, and the orphan arm on a missing `warn` after burning
its 5s ceiling — a red that names the absent log line rather than the leak. Both
are expected on a revert; neither indicates a broken test.
