# Cancellation-safe `BEGIN IMMEDIATE` — implementation plan

Issue #592 · spec: `docs/workflow/specs/2026-08-27-cancelled-begin-immediate-lock-leak-design.md` · ADR 0087
Governing rules: ADR 0083 (read-then-write ⇒ `BEGIN IMMEDIATE`), ADR 0086 (four named openers).
ADR 0068 is **not** superseded: the `init.rs` rewrite that would have superseded its
`conn.begin_with` mechanism sentence is cut from this plan (see *Not done here*).
Deferrals in this diff: `docs/debt/0005-connection-level-custom-begins-are-unguarded.md`
(which owns both the live `init.rs:54-56` site and the guardrail blind spot) and
`docs/debt/0006-run-constrained-leaks-load-hogs.md`.

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
  `tracing-subscriber` joins `voom-store` **and** `voom-node-agent` as a
  **dev**-dependency only — the second so the acceptance sweep's test binary has
  a subscriber to emit through at all (T1b). It is a
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
`tracing-subscriber` 0.3.23 is already in the graph for three other crates. T1b
adds a second such line to the `voom-node-agent` package entry, by the same
mechanism and with the same absence of a new `[[package]]` block; those two lines
are the whole of this change's lock diff.

## T1b — `lifecycle.rs` gains the log subscriber the sweep reads

Files: `crates/voom-node-agent/tests/lifecycle.rs`,
`crates/voom-node-agent/Cargo.toml`.

**It runs here, before T2b, and the position is load-bearing.** An earlier draft
placed this task after T4, which put it *between* the two acceptance arms: T2b
would have measured the pre-fix binary without a subscriber and the post-fix arm a
binary with one. The whole confidence argument treats those two arms as one
experiment differing in a single variable — the fix — and a differing dev-
dependency, a differing linked subscriber and differing stderr volume under a
`--write-bps 40M` throttle break that. Landing it before both arms costs nothing:
the plan's own Rollback section establishes this task is inert without the fix, so
it cannot contaminate the pre-fix measurement it now precedes.

Without this task the acceptance sweep's orphan-`warn` count is `0` on every run
whatever the code does, and the operator's budget-ladder exclusion is conditioned
on that count. The spec carries the full argument; the verified facts are that
`crates/voom-node-agent/Cargo.toml` declares no `tracing` and no
`tracing-subscriber` edge in either section, that the workspace's only global
subscribers are `crates/voom-api/src/main.rs:54` and
`crates/voom-cli/src/logging.rs:21,28` — neither linked into this test binary —
and that `voom-test-support` has no `tracing` edge to supply one transitively.

`Cargo.toml` gains `tracing-subscriber` as a **dev**-dependency
(`tracing-subscriber.workspace = true` under `[dev-dependencies]`; 0.3.23 is
already in the workspace graph).

`lifecycle.rs` installs exactly one global subscriber, from a `OnceLock` so that
concurrent test entry cannot race and a second install cannot panic, writing to
`io::stderr()` with timestamps and an `EnvFilter` defaulting to
`voom_store=warn`. `HANG_GUARD` stays at 30s, every wait it bounds stays where it
is, and no assertion changes.

**Verify the instrument before trusting it, in this task and not in the sweep.**
Add a temporary `tracing::warn!` carrying the sweep's exact predicate string —
`completed after its caller was cancelled` — to `begin_read_then_write` in
`crates/voom-store/src/tx.rs`, which exists now and which the lifecycle test
already drives. Run one lifecycle test the way the sweep runs it
(`--test-threads=1 --nocapture`, output redirected to a file), confirm the line is
in that file for a **passing** test, then revert the temporary line. `tx.rs` is
the right site because `begin_detached` does not exist until T3 and this task
deliberately precedes it; what is being proved is that a `voom_store` `warn`
reaches the sweep's log, which does not depend on which function emitted it.

A subscriber that installs but whose output never reaches the log is the same
disconnected instrument in a new place, and the sweep cannot tell the difference —
which is the entire defect this task exists to close.

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
  written in the test, **3 repeats**, at least one blocked observation across
  them. Gated on `available_parallelism() >= 4` as a **silent early return**; the
  fixed arm is **not** gated. It was written as 5 here and cut to 3 by the
  measured-result clause above — see *T2 measured result* below for the wall clock
  that forced it and the miss rate it costs.

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
costs nothing extra — the openers are untouched at T2 — whereas running it after
T6 needs a revert, a `git stash`, or
`git worktree add ../voom-592-prefix <merge-base>`, and each of those invalidates
the `llvm-cov` instrumented build, adding two full instrumented rebuilds of
`voom-node-agent` and its graph to a protocol already priced at ~180 runs.

**Both arms run the same test binary, and that is why T1b precedes this.** The
subscriber and its dev-dependency are already in place here, so the pre-fix and
post-fix arms differ in exactly one variable — the fix. Had T1b run between them,
the comparison would have crossed a changed dependency graph, a changed linked
subscriber and changed stderr volume under a `--write-bps 40M` throttle, and the
reproduction index measured here would not have transferred to the post-fix arm
whose confidence rests on it.

Run the loop from *Acceptance run* below, unfixed, into `.tmp/accept-prefix/`.
Stop at the first log matching the predicate, or at 90 runs. Record the
**reproduction index** — the run number that first reproduced — because that
index is the host-local evidence the whole post-fix sweep's confidence rests on.
If 90 runs do not reproduce, criteria 2, 4 and 5 are reported **not discharged at
the stated confidence** per *Decide the bar now*, and that verdict is reached here
rather than discovered at the end.

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

## Not done here — `init.rs`

An earlier draft of this plan routed `run_migrations_on` through
`begin_read_then_write`, and the third scope audit was right that nothing
authorized it. No completion criterion requires it, and `run_migrations_on` is
not on the deadlocked control path: its only production caller is
`crates/voom-cli/src/commands/system/init.rs:16` (every other caller is a test,
and `init_on` is behind the `test` feature by construction at `init.rs:31`), so
it sits behind neither `bounded_router`'s `TimeoutLayer` nor an axum client
disconnect — the two cancellation sources this design identifies. A CLI
invocation's cancellation is SIGINT, which takes the process and the file lock
with it.

The charter grants `crates/voom-store/` as *surface*, but surface says where a
change may land, not what may change. Absorbing a second production rewrite, a
superseded sentence of ADR 0068, and the loss of the
`acquire connection for migration` error context on this run's own authority is
scope this quest cannot grant itself.

Cutting it is not free, and the cost is paid in `docs/debt/0005` rather than
left implicit: that record's "Why deferred" rested on "The live instance is
fixed", which is no longer true, so it is rewritten to own the live
`init.rs:54-56` site *and* the `check-transaction-openers.sh` blind spot. The
live site therefore has a named owner and a `review-by` date instead of
disappearing.

Two consequences follow for the rest of this plan. T6's non-regression predicate
returns `init.rs:55` **as well as** `tx.rs`, so it is stated as that exact set
rather than "only `tx.rs`". And the spawn-latency gap on the migration path is
gone with the task that would have introduced it — `run_migrations_on` keeps its
inline `pool.acquire()` and `conn.begin_with`, so
`busy_timeout_exhaustion_surfaces_database_error` (`init_test.rs:297`) and
`locked_migration_true_race_reports_zero_applied` (`:268`) keep the internal
acquire their setups assume, untouched.

## T6 — Guardrails and the record

Files: none beyond what T0–T4 touched, except `docs/debt/0005`, whose "Why
deferred" is rewritten to own the live `init.rs` site now that the `init.rs`
rewrite is cut. `docs/` already carries ADR 0087, its index row, and the spec.

- `./scripts/check-transaction-openers.sh crates` still exits 0, and the
  non-regression boundary is this determinate predicate:

  ```
  rg -n 'begin_with' crates --type rust \
    -g '!**/tests/**' -g '!**/*_test.rs' -g '!crates/voom-test-support/**'
  ```

  returns matches in exactly two files — `crates/voom-store/src/tx.rs` and
  `crates/voom-store/src/init.rs` — and no others. **Stated at file granularity,
  and deliberately.** A line-level expectation does not survive this change: today
  the predicate returns `tx.rs:5` (a module doc comment), `tx.rs:40`, `tx.rs:105`
  and `init.rs:55`, but T3 routes `:40` and `:105` through `begin_detached`, so
  neither line contains `begin_with` afterwards. What matches in `tx.rs` post-T3
  is the doc comment plus the single `pool.begin_with(statement)` inside
  `begin_detached` — two lines, at different numbers. `init.rs:55` is unchanged
  and **expected**: with the `init.rs` rewrite cut from this plan, that live site
  is owned by `docs/debt/0005` rather than removed, and a predicate demanding
  `tx.rs` alone would go red on the tree this change actually produces.
  `docs/debt/0005` states the boundary at file granularity for exactly this
  reason.

  The `crates/voom-test-support/**` glob is load-bearing **even though nothing it
  excludes appears in either output** — that is the point of it. Drop the glob and
  `commit_node.rs:89,110` and `staging_seed.rs:59` reappear: `src/` files in a
  support crate that `check-transaction-openers.sh` exempts through its
  `grep -Ev "/(voom-test-support|voom-fakes|…)/"` filter rather than its test-file
  filter, and untouched by this change. That is what the earlier wording — "only
  `tx.rs` and test files" — could not express, which is why it could not
  distinguish success from failure. `docs/debt/0005` carries this predicate under
  its Non-regression boundary section; T6 re-runs it and confirms the result.
  That record's **expected file set is updated to name `init.rs` alongside
  `tx.rs`** in the same edit that rewrites its "Why deferred", since the live site
  now stays.
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

**The hog predicate is anchored, and an earlier draft of this plan was not.**
`pgrep -f` and `pkill -f` match the *whole* command line of every process, and the
shell running this loop has the pattern in its own command line — the `pkill` line
below contains it literally. Measured on this host with one real hog alive:
`pgrep -cf 'sh -c while :; do :; done'` returns **2** (the hog plus this loop's own
shell), while `pgrep -cf '^sh -c while :'` returns **1**, the hog alone. Unanchored,
the guard therefore aborts on run 1 of a 90-run arm with a leak that is not there,
and the unanchored `pkill` is worse than useless: its match set includes the shell
executing the sweep, so the reap can kill the run it is protecting. Anchoring at
`^sh` fixes both, because the hogs are spawned as literal `sh -c while :; do :; done`
while any wrapping shell's command line begins with its own interpreter path.
Verified after the change: the anchored `pkill` removed the test hog and left the
invoking shell alive.

```
# $ARM is accept-prefix (T2b) or accept-postfix; each arm gets its own directory
# so the second cannot overwrite the first arm's evidence. `.tmp*/` is already
# gitignored; the repo root is not, and `*.log` is not ignored anywhere.
mkdir -p .tmp/$ARM
hogs() { pgrep -cf '^sh -c while :' || true; }   # anchored; see below
executed=0
for i in $(seq 90); do
  [ "$(hogs)" -eq 0 ] || { echo "ABORT: $(hogs) leaked hogs before run $i"; break; }
  log=.tmp/$ARM/run-$i.log
  ./scripts/run-constrained.sh --load 1 --write-bps 40M -- \
    cargo llvm-cov --no-report -p voom-node-agent --test lifecycle \
    --all-features -- --test-threads=1 --nocapture >"$log" 2>&1
  rc=$?
  pkill -f '^sh -c while :'                     # the script will not
  # llvm-cov --no-report leaves .profraw beside the crate, and they are not
  # gitignored. Unswept, 90 runs bury `git status` in hundreds of untracked
  # files and a later `git add -A` sweeps them into a commit.
  find crates -name '*.profraw' -delete
  case $rc in
    0|101) ;;                                    # ran; 101 is a real test failure
    *) echo "ABORT: run $i exited $rc — not a run"; break ;;
  esac
  grep -q 'test result:' "$log" || { echo "ABORT: run $i never ran the suite"; break; }
  executed=$((executed + 1))
  # BOTH branches. begin_detached emits two different messages and the failure
  # branch is the one that occupied a connection longest — see below.
  orphans=$(grep -cE 'after its caller was cancelled|for a caller that was already cancelled' "$log" || true)
  failed=$(grep -c 'for a caller that was already cancelled' "$log" || true)
  echo "run $i: orphan_warns=$orphans orphan_failed=$failed"
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

**Count both orphan branches, and report the failure branch separately.**
`begin_detached` emits two different messages: the `Ok` branch reads *"transaction
open completed after its caller was cancelled; rolling back"*, the `Err` branch
*"transaction open failed for a caller that was already cancelled"*. An earlier
predicate here matched only the first, and that is the wrong half to lose. The `Err`
branch is reached when `pool.begin_with` returns an error for an already-cancelled
caller, and the dominant errors under this sweep's own conditions are `SQLITE_BUSY`
after the full 30s `busy_timeout` and `PoolTimedOut` after the 45s `acquire_timeout`
— the orphan that held a pooled connection for the *entire* wait, which is precisely
the maximum-occupancy case the `--write-bps 40M` throttle exists to produce. The
counted `Ok`-branch orphan is one whose open succeeded, which uncontended means it
held the connection for microseconds. Under the old predicate a run in which the
residual behaved worst could report the lowest count.

**The count is worthless unless the subscriber is wired, which is T1b's job.**
Without it `tracing::warn!` emits no bytes and this `grep` returns `0` on every
run whatever the code does — `orphan_warns=0` across 90 runs reading as *no
orphans occurred* while actually meaning *nothing was listening*. T1b installs the
subscriber and proves it with one real emitted line before the sweep starts; do
not start the sweep on an unverified instrument. `--nocapture` above is the
second belt: a `fmt` layer holding `io::stderr()` should survive libtest's
capture of a passing test, but the sweep does not need that argument to be right.

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

**Decide the bar now, not after the sweep.** Criteria 2, 4 and 5 have no
deterministic discharge path in this design — that is a property of a
1-in-20-to-30, throttle-dependent defect, not a defect of the plan. So: if the
pre-fix arm does not reproduce within 90 runs, **proceed** on the mechanism test
alone and report criteria 2, 4 and 5 as *not discharged at the stated confidence*,
naming the executed count and the host. Do not block on it, and do not quote
0.047 unconditionally in the PR.

**Criterion 4 is in that list, and an earlier draft of this plan left it out.**
"`HANG_GUARD` in `crates/voom-node-agent/tests/lifecycle.rs` is not raised" is
discharged by exactly one thing: the post-fix arm completing `executed=90` with no
match for `second agent graceful-shutdown lifecycle did not complete`. That is the
same evidence and the same conditional confidence as criterion 5's second clause,
so it inherits the same caveat — a sweep that never reproduced pre-fix cannot
distinguish "the guard no longer fires" from "the window was never entered". The
omission mattered because the guard not firing is the *default* observation: a PR
following the earlier draft literally would have caveated criteria 1, 2 and 5 and
reported criterion 4 as cleanly discharged, on evidence this plan's own argument
says is inconclusive. Report it beside criterion 5, never on its own.

`just ci` going green is **not** criterion 4's discharge either. One unthrottled
green run is exactly the signal the spec already rejects for criterion 5, for the
same reason: at a 1-in-20-to-30 rate it is indistinguishable from a run that never
entered the window.

**Criterion 1 is named in that same failure case, and this is the sentence that
does it.** The charter sets criterion 1's terms: discharged "by
mechanism-from-sources plus isolated reproduction, with attribution to this
issue's own reproduction carried by the before/after acceptance sweep rather than
by direct observation". A non-reproducing pre-fix arm removes the attribution
half specifically, and reporting criteria 2 and 5 as undischarged while reporting
criterion 1 as simply *discharged* would overstate it against the terms the
operator set — in the report a later reader uses to decide whether #592 is
actually closed. So in that case the PR says, in these words: **criterion 1 is
discharged by mechanism-from-sources plus isolated reproduction, with no
before/after attribution to #592's own reproduction.** The mechanism half stands
on the `sqlx` 0.8.6 sources and the deterministic isolated reproduction and is
not weakened by the sweep; only the attribution half is lost, and it is lost
visibly.

**The post-fix arm runs either way.** An earlier draft made the whole sweep
conditional on the pre-fix arm reproducing, which left a hole: the orphan-`warn`
counts are the *only* evidence about the residual this design adds on criterion
2's path, and that draft permitted the run to end having never gathered them. The
two arms answer different questions and are decoupled accordingly — the pre-fix
arm calibrates the *confidence* attachable to criterion 5, and the post-fix arm
supplies the residual evidence regardless of what the pre-fix arm found. **A
`executed=90` post-fix arm is required before criterion 2 is reported at all**,
discharged or not.

**T2 measured result, as this plan's wall-clock budget above required.** On the
design host (48 cores, debug, uninstrumented): default parallel 12.3-12.8s;
`taskset -c 0-3` 11.6-14.3s; `--test-threads=1` **19.8s** with five control repeats.
The serialized figure is the one that governs, because the `coverage` job runs
`--test-threads=1` under `cargo llvm-cov`, and 19.8s is 32% above this plan's own
15s threshold *before* instrumentation — in the job whose duration is this issue's
subject. So the budget clause was exercised rather than waived: `CONTROL_REPEATS`
is 3 while the fixed arm keeps 5, which brings the serialized figure to **14.9s**
(three runs: 15.53s, 14.99s, 15.03s wall including build check; re-measured later
at 15.28s binary time). The control
dominates because at a leaking *N* its observer necessarily burns its whole
ceiling; the fixed arm's *N* values return promptly. At three repeats the
per-sweep miss rate is 0.275^3 = 0.021.

The figure sits *at* the 15s threshold rather than under it, and the honest
reading is that the threshold was met by rounding. If it is revisited, the
repeats are the wrong place to look a second time: post-fix the fixed arm does
real work only at *N* = 1, so 35 of its 40 iterations each pay a fresh database,
pool, warm-up and settle to assert something that cannot fail. Narrowing its
sweep would buy back more than the control's repeats did and would let the
control return to 5 and its 0.0016 miss rate. That is a change to the test's
shape, so it is recorded here rather than taken.

**The orphan counts cover the tests a subscriber was installed for.** An earlier
version of `lifecycle.rs` called `init_tracing()` from one test, and libtest runs
tests in name order, so `delayed_acquire_replay_never_dispatches` — which also
starts a `LiveFixture` and drives the same openers — ran *before* any subscriber
existed and any orphan it produced was invisible. That is the T1b defect one scope
narrower: an instrument not connected where the count is read. The call is now in
`LiveFixture::start`, so every test that can orphan an open is instrumented. The
500-run counts below were gathered before that change and therefore cover
`live_agent_fences_prior_incarnation_and_retires_orderly` only; they are a lower
bound over the binary, not a count of the whole run.

**The sweep result, recorded here because `.tmp/` is gitignored and this plan
required it reported.** Both arms ran #592's recipe under `run-constrained.sh`
(`--load 1 --write-bps 40M`, `--test-threads=1`, `cargo llvm-cov --no-report`) on
the design host, 48 cores.

| arm | tree | executed | reproduced | orphan `warn`s |
|---|---|---|---|---|
| pre-fix | openers unfixed | 90 | **yes, at run 90** | 0 (`begin_detached` did not exist) |
| post-fix | `904982e0` | **500** | no | 38 total, per-run max 1 |

**The reproduction index is 90 of 90, and it changes the arithmetic.** The
stopping rule was "first reproduction or 90 runs, whichever comes first", so the
arm reproduced on its last permitted run. The maximum-likelihood host rate is
therefore about **1/90**, roughly three times rarer than the 1-in-20-to-30 the
issue records and below the `p >= 1/30` that this plan's `(29/30)^90 = 0.047`
assumes. **That figure does not apply and must not be quoted.**

What replaces it is a stronger result reached by a different route: the post-fix
arm ran 500 runs rather than the 90 required, so at `p = 1/90` the conditional
false-negative probability is `(89/90)^500` = **0.004**. Criteria 2, 4 and 5 are
discharged against that figure and no other. Ninety post-fix runs at the measured
rate would have left 0.37 — which is why the extra runs were bought.

Criterion 4 specifically: `HANG_GUARD` is what bounds
`wait_for_graceful_shutdown`, so the pre-fix arm's single reproduction *is* a
`HANG_GUARD` expiry — that is the defect, not a separate observation. Post-fix,
none of the 500 runs failed at all (`grep -l 'test result: FAILED'` matches 0 of
500; pre-fix it matches exactly `run-90.log`). The guard not firing is the
default observation, so it carries only the confidence the executed count above
supports.

**Re-derived after the predicate was fixed, from the retained logs rather than a
re-run.** The post-fix sweep at `904982e0` had already executed 500 runs under the
old `Ok`-branch-only predicate. Recounting `.tmp/accept-postfix/*.log` for both
branches gives **`Ok` 38, `Err` 0, total 38**, with a per-run maximum of 1 across all
500 runs — identical to what the incomplete predicate reported, because the failure
branch never fired in this sweep. So the reported result stands as an upper bound,
and it now stands on a count that could have found the other class had it occurred.
The pre-fix arm's logs contain no orphan lines of either kind, as expected:
`begin_detached` did not exist on that tree.

**Pre-commit to the split threshold.** If any single run reports **more than one
orphan `warn`**, that is the multi-opener occupancy the spec names. Decide that
now so the result cannot be read charitably after the fact. A fully successful run
of this plan can legitimately end with two of five criteria undischarged; that is
the plan working, and the PR body must say so in those words rather than implying
a green sweep.

**Tripping the threshold does not file anything.** The charter's wording is "opens
a follow-up issue", but this plan does not create one automatically: the sweep
reports the per-run counts and states plainly whether the threshold was crossed,
in the PR body and the hand-off, and the operator decides whether an issue is
opened. Two reasons, and either is sufficient. Creating an external artifact needs
authority this run does not hold for a threshold it restated (below). And the
number alone cannot distinguish sustained occupancy from a clustered burst, which
is the judgement an issue would have to encode. Pre-committing to the *threshold*
is what stops the result being argued away; pre-committing to the *filing* would
be this run acting on the operator's behalf on the one point it changed.

The charter words the threshold as *"more than one **concurrent** orphan in any
run"*, and the restatement above is deliberate: `grep -c` over a whole run's log
yields one integer with no timestamps and no paired release event, so it cannot
tell two overlapping orphans from five spread across the run. Deciding which had
occurred after seeing the number is exactly the after-the-fact reading this
paragraph exists to prevent. Counting per run is strictly more sensitive — every
concurrent pair is also two in a run — so it can only trip earlier, never suppress
a signal the operator asked for. The spec records the restatement against the
charter, and the hand-off surfaces it as a threshold this run changed rather than
burying it.

**Do not run this sweep concurrently with any other `run-constrained.sh`
invocation on the same host.** The reap above is `pkill -f` on a command-line
pattern, which matches host-wide rather than by process group — the hogs are
reparented to init by the script's own `exec`, so they are not in this loop's
process tree and cannot be reaped from it. A concurrent invocation would silently
lose its load generators mid-run.

Up to ~180 instrumented, throttled, serialized lifecycle runs, **measured at
~20s each on the design host** — roughly an hour for the full protocol. If it is
not run
to completion, say so and say which criteria that leaves undischarged.

## Rollback

`git revert` the **T2, T3 and T4** commits — the fix *and* the tests.
`begin_detached` is additive and the two openers keep their signatures, so
reverting restores the prior behaviour without touching any of the ~50 call
sites. T1 stays: the manifest change genuinely is inert, and `rt` is enabled
through sqlx regardless. T1b stays too — a log subscriber in a test binary is
inert with the fix reverted, and reverting it would only re-break the
instrument.

The tests must go with the fix. They exist only to prove it and are meaningless
without it — reverting T3 alone leaves **both** arms red (the fixed arm on a
leaked lock, the orphan arm on a missing `warn` after burning its 5s ceiling)
and therefore `just test` and `just ci` permanently failing. That is a rollback
plus a broken gate, in a section that gets read under time pressure after
something went wrong.
