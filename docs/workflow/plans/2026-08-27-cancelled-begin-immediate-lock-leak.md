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
  `tracing-subscriber` joins `voom-store` as a **dev**-dependency only. It is a
  fourth direct dependency where the charter's surface bullet names three
  (`sqlx`, `axum`/`hyper`, `tokio`), so the judgement is recorded rather than
  assumed: `crates/voom-store/` is in the frozen surface and its manifest with
  it; `AGENTS.md:175` states dev-dependencies "are deliberately excluded from the
  production map" and already sanctions voom-store's dev-only edges against the
  normal layering; and `tracing-subscriber` 0.3.23 is already in the workspace
  graph for `voom-api`, `voom-control-plane` and `voom-cli`, so this adds an edge
  and no new supply-chain code. The `Cargo.lock` line at the repository root is
  an unavoidable consequence of an in-surface manifest change. The alternative —
  hand-rolling a `tracing::Subscriber` in the test file — is more code in the
  least-settled file. Take the dependency.
- **The spec's test design is not settled, and the latitude that grants is
  bounded here.** Its review cycle exited at budget with the Testing section
  rewritten three times, each rewrite producing new defects in the next pass.
  Treat T2–T4 as the spec's *intent* and verify each arm bites (below). But the
  implementer does **not** hold open-ended design authority over it:

  - **Arm structure returns to the spec.** Changing the number of arms, what an
    arm asserts, or the parallelism gate is a design change: stop, amend the
    spec, and do not land it through the branch review. The structure is the part
    that is well-argued — each arm is earned by something the others cannot
    prove — and it is not what churned.
  - **Constants stay with the implementer.** The settle, the two ceilings, and
    the repeat count are already parameterised here with stated grounds; tune
    them against measurement and record what changed. That is what churned, and
    it is tuning, not design.

  The branch review at quest step 6 remains the check on the implemented test.
  This bullet says what it may not be asked to settle.

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

**The `rt` addition is declarative and no compilation can falsify it.** Two
earlier drafts of this plan argued about which `cargo check` invocation detects an
omitted `rt`; both were wrong, and the answer is that none does and none needs to.
`sqlx-core` 0.8.6 declares its tokio dependency with
`features = ["time","net","sync","fs","io-util","rt"]`
(`sqlx-core-0.8.6/Cargo.toml:192-201`), and `sqlx`'s `runtime-tokio` — which the
workspace pin enables — activates it through `sqlx-core/_rt-tokio`. voom-store
depends on `sqlx` unconditionally, so `rt` is already on the single unified tokio
build in the crate's **normal, no-dev** graph. Measured:
`cargo tree --locked -p voom-store -e no-dev -f '{p} | {f}'` reports
`tokio v1.53.1 | bytes,default,fs,io-util,libc,mio,net,rt,socket2,sync,time`.

Add the feature anyway: declaring a tokio API the crate calls directly is correct
hygiene and costs nothing, and it stops the build depending on sqlx continuing to
enable `rt` for us. But do not write a verify step for it — there is nothing to
observe.

Verify: `cargo check -p voom-store --all-features --all-targets`, which proves the
`tracing-subscriber` dev-dependency resolves and compiles. Then `just deny`,
`just audit`.

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

**This is a sibling fix, not part of the deadlock remedy, and the PR says so.**
No completion criterion requires it. `run_migrations_on` is not on the deadlocked
control path: its only production caller is
`crates/voom-cli/src/commands/system/init.rs:16` (every other caller is a test,
and `init_on` is behind the `test` feature by construction at `init.rs:31`), so it
sits behind neither `bounded_router`'s `TimeoutLayer` nor an axum client
disconnect — the two cancellation sources this design identifies. A CLI
invocation's cancellation is SIGINT, which takes the process and the file lock
with it.

It is kept because it shares the *verified* root cause, sits inside the frozen
surface, is small, and is what makes `docs/debt/0005` a pure recurrence guard.
**The two are coupled and cannot be dispositioned independently:** if T5 is
dropped, `docs/debt/0005`'s "Why deferred" section must be rewritten, because it
rests on "The live instance is fixed: ADR 0087's change routes `init.rs` through
`begin_read_then_write`, so no production `conn.begin_with` remains."

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
`crates/voom-store/src/init_test.rs`. **Two** tests there call
`run_migrations_on` and each encodes an assumption about its *internal* acquire
that T3 moves onto a spawned task:

- `busy_timeout_exhaustion_surfaces_database_error` (`:297`) — its own comment
  calls the setup load-bearing: `conn2` is returned to the idle queue so
  `run_migrations_on`'s internal `pool.acquire()` reuses that specific
  `busy_timeout = 0` connection. Post-T3 that acquire happens inside
  `begin_detached`'s spawned task against the same pool, so it should still draw
  the same idle connection — but this is the test that says otherwise if it does
  not.
- `locked_migration_true_race_reports_zero_applied` (`:268`) — the held-lock race
  ADR 0068 exists for.

An earlier draft of this plan also listed `single_shot_replacement_has_no_polling`
(`:214`) as a wall-clock guard on this path. It is not: that test never calls
`run_migrations_on`: it inlines its own `pool.acquire()`, `conn.begin_with`,
`run_direct`, drops and `probe_schema`, and its `<25ms` assertion bounds that
inline sequence. It stays green whatever T5 does.

**So nothing bounds the latency the task spawn adds to the migration path**, and
that is accepted rather than covered: the addition is one `tokio::spawn` plus one
`oneshot` round trip on a path that already performs a pooled acquire and runs
migrations. Stated here so the gap is a decision rather than an oversight.

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

  returns only `crates/voom-store/src/tx.rs` — **stated at file granularity, and
  deliberately.** A line-level expectation does not survive this change: today the
  predicate returns `tx.rs:5` (a module doc comment), `tx.rs:40`, `tx.rs:105` and
  `init.rs:55`, but T3 routes `:40` and `:105` through `begin_detached`, so
  neither line contains `begin_with` afterwards. What matches in `tx.rs` post-T3
  is the doc comment plus the single `pool.begin_with(statement)` inside
  `begin_detached` — two lines, at different numbers. `docs/debt/0005` states the
  boundary at file granularity for exactly this reason.

  The `crates/voom-test-support/**` glob is load-bearing **even though nothing it
  excludes appears in either output** — that is the point of it. Drop the glob and
  `commit_node.rs:89,110` and `staging_seed.rs:59` reappear: `src/` files in a
  support crate that `check-transaction-openers.sh` exempts through its
  `grep -Ev "/(voom-test-support|voom-fakes|…)/"` filter rather than its test-file
  filter, and untouched by T5. That is what the earlier wording — "only `tx.rs`
  and test files" — could not express, which is why it could not distinguish
  success from failure. `docs/debt/0005` **already carries this exact predicate**
  under its Non-regression boundary section; T6 re-runs it and confirms the
  result. Leave that record unmodified — T6 touches no files.
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

**The loop goes outside `run-constrained.sh`** — but not for the reason an
earlier draft gave. `IOWriteBandwidthMax` (`run-constrained.sh:174`) maps to
cgroup v2 `io.max wbps`, a continuously enforced **rate limit with nothing to
deplete**, so 90 sequential runs inside one scope would each get the same 40M/s
that 90 separate scopes give them. The throttle is unaffected by placement. The
outer loop is chosen because each run gets its own `MemoryMax` accounting and its
own load setup, and because it is the exact per-run invocation #592 records as
reproducing.

**Reap the load hogs between runs.** `run-constrained.sh` leaks them:
`trap cleanup EXIT INT TERM` at `:150`, hogs spawned at `:152-160`, then `exec
systemd-run …` at `:177` replaces the shell and discards the trap, so every
`--load` process is reparented to init and spins forever. Reproduced on this
host: `./scripts/run-constrained.sh --cpus 0 --load 1 -- true` returns 0 and
leaves `sh -c while :; do :; done` alive. At the protocol's defaults (`--cpus
0-3`, `--load 1`) that is **four orphans per invocation** — ~360 spinning
processes by the end of the pre-fix arm, with the post-fix arm starting on top of
them. The sweep's runs would get monotonically heavier, which destroys the very
comparability the outer placement is for, and CPU starvation can trip
`HANG_GUARD` for reasons unrelated to the defect — indistinguishable in the log,
because the predicate *is* a `HANG_GUARD` message. `run-constrained-selftest`
does not cover this: `--print-plan` exits at `:128`, before the hog block.

`scripts/` is outside this issue's frozen surface, so the loop compensates rather
than fixing the script, and the script bug is filed as
`docs/debt/0006-run-constrained-leaks-load-hogs.md`.

```
# $ARM is accept-prefix (T2b) or accept-postfix; each arm gets its own directory
# so the second cannot overwrite the first arm's evidence. `.tmp*/` is already
# gitignored; the repo root is not, and `*.log` is not ignored anywhere.
mkdir -p .tmp/$ARM
hogs() { pgrep -cf 'sh -c while :; do :; done' || true; }
executed=0
for i in $(seq 90); do
  [ "$(hogs)" -eq 0 ] || { echo "ABORT: $(hogs) leaked hogs before run $i"; break; }
  log=.tmp/$ARM/run-$i.log
  ./scripts/run-constrained.sh --load 1 --write-bps 40M -- \
    cargo llvm-cov --no-report -p voom-node-agent --test lifecycle \
    --all-features -- --test-threads=1 >"$log" 2>&1
  rc=$?
  pkill -f 'sh -c while :; do :; done'          # the script will not
  case $rc in
    0|101) ;;                                    # ran; 101 is a real test failure
    *) echo "ABORT: run $i exited $rc — not a run"; break ;;
  esac
  grep -q 'test result:' "$log" || { echo "ABORT: run $i never ran the suite"; break; }
  executed=$((executed + 1))
  orphans=$(grep -c 'completed after its caller was cancelled' "$log" || true)
  echo "run $i: orphan_warns=$orphans"
  if grep -q 'second agent graceful-shutdown lifecycle did not complete' "$log"
  then echo "reproduced at run $i"; break; fi
done
echo "executed=$executed"
```

**Count the orphan `warn`s per run.** One `grep` over logs already being written,
and it is the only evidence anyone will have about the residual this design
introduces. ADR 0087 accepts that a caller cancelled inside `acquire()` now leaves
a detached task holding a pool slot for up to `LOCK_WAIT_BUDGET`, worst-case 75s
end to end — which **exceeds the node agent's 30s per-attempt timeout** and is a
large fraction of its 153.75s budget, on exactly the path completion criterion 2
is judged on. No measurement of that case exists, here or anywhere. The sweep
runs the real workload under the real throttle, so it is where "occupancy decays"
stops being an argument and becomes an observation. Report the counts.

**Every iteration must prove it ran**, because the grep alone cannot tell "90
clean runs" from "90 runs that never started". `run-constrained.sh` exits 2 or 3
on six preconditions (`:132-141`, `:169-174` — not Linux, no `systemd-run`, no
`taskset`, cgroup v1, memory controller not delegated, unresolvable device), and
a compile error, an OOM kill under `MemoryMax=16G`, or a transient scope failure
all exit non-zero with no `HANG_GUARD` text. Without the `rc` check and the
`test result:` requirement, the **post-fix arm's success condition — ≥90 runs,
no match — is satisfied exactly by 90 instantaneous failures**, and criteria 2
and 5 get reported discharged on a sweep that never ran. The partial case is
likelier and quieter: a few failed iterations shrink the real run count while the
loop still counts to 90, inflating the stated confidence.

Report `executed` next to the 90 the (29/30)^90 figure assumes. If they differ,
the figure does not apply.

The `break` on a match is what makes T2b's stopping rule executable; without it
the pre-fix arm runs all 90 regardless. The post-fix arm runs the same loop with
`$ARM=accept-postfix`, must reach `executed=90`, and treats any match as a
**failure**, not a stop.

Report both counts and the pre-fix reproduction index. The (29/30)^90 = 0.047
figure is conditional on #592's rate transferring to this host, and that index is
the host-local evidence for it.

**Decide the bar now, not after the sweep.** Criteria 2 and 5 have no
deterministic discharge path in this design — that is a property of a
1-in-20-to-30, throttle-dependent defect, not a defect of the plan. So: if the
pre-fix arm does not reproduce within 90 runs, **proceed** on the mechanism test
alone and report criteria 2 and 5 as *not discharged at the stated confidence*,
naming the executed count and the host. Do not block on it, and do not quote
0.047 unconditionally in the PR.

**The post-fix arm runs either way.** An earlier draft made the whole sweep
conditional on the pre-fix arm reproducing, which left a hole: the orphan-`warn`
counts are the *only* evidence about the residual this design adds on criterion
2's path, and that draft permitted the run to end having never gathered them. The
two arms answer different questions and are decoupled accordingly — the pre-fix
arm calibrates the *confidence* attachable to criterion 5, and the post-fix arm
supplies the residual evidence regardless of what the pre-fix arm found. **A
`executed=90` post-fix arm is required before criterion 2 is reported at all**,
discharged or not.

**Pre-commit to the split.** If any run reports more than one concurrent orphan
`warn`, that is the sustained multi-opener occupancy the spec names, and it opens
a follow-up issue rather than being absorbed here. Decide that now so the result
cannot be read charitably after the fact. A fully successful run of this plan can
legitimately end with two of five criteria undischarged; that is the plan working,
and the PR body must say so in those words rather than implying a green sweep.

**Do not run this sweep concurrently with any other `run-constrained.sh`
invocation on the same host.** The reap above is `pkill -f` on a command-line
pattern, which matches host-wide rather than by process group — the hogs are
reparented to init by the script's own `exec`, so they are not in this loop's
process tree and cannot be reaped from it. A concurrent invocation would silently
lose its load generators mid-run.

Up to ~180 instrumented, throttled, serialized lifecycle runs. If it is not run
to completion, say so and say which criteria that leaves undischarged.

## Rollback

`git revert` the **T2, T3, T4 and T5** commits — the fix *and* the tests.
`begin_detached` is additive and the two openers keep their signatures, so
reverting restores the prior behaviour without touching any of the ~50 call
sites. T1 stays: the manifest change genuinely is inert, and `rt` is enabled
through sqlx regardless.

The tests must go with the fix. They exist only to prove it and are meaningless
without it — reverting T3 and T5 alone leaves **both** arms red (the fixed arm on
a leaked lock, the orphan arm on a missing `warn` after burning its 5s ceiling)
and therefore `just test` and `just ci` permanently failing. That is a rollback
plus a broken gate, in a section that gets read under time pressure after
something went wrong.
