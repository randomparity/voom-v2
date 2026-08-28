# Implementation plan — bounded node-agent shutdown (#452)

**Goal.** Make a node agent stopped with a single `SIGTERM` exit within a
published wall-clock bound even when the control plane is unresponsive, instead
of waiting out the client's 153.75 s retry budget on each of two shutdown-tail
control-plane calls.

**Architecture.** `crates/voom-node-agent` supervises out-of-process workers. One
`AgentRuntime` owns a `JoinSet` of per-worker coordinator tasks; each coordinator
settles its held leases against the control plane, then reaps its child process.
When every coordinator has joined, the runtime deactivates the incarnation — the
write that records `Retired`/`GracefulShutdown`. This change bounds each of those
two control-plane waits, bounds the post-`SIGKILL` reap, releases two coordinator
waits that no shutdown receiver races, and wraps the whole tail in a backstop
timeout sized above the sum.

**Tech stack.** Rust 2024 edition, tokio (multi-thread runtime, `sync`, `time`,
`process`, `signal`), `async-trait`. Tests are `#[tokio::test]`, many with
`start_paused = true`.

## Global Constraints

Transcribed from `AGENTS.md` and
`docs/workflow/specs/2026-08-28-bounded-node-agent-shutdown-design.md`. Every task's
requirements implicitly include this section.

- **Unit tests live in a sibling file** `<source>_test.rs`, linked from the parent
  with `#[cfg(test)] #[path = "foo_test.rs"] mod tests;`. Never an inline
  `#[cfg(test)] mod tests { … }` in `src/`. Enforced by `just check-test-layout`.
- **Never pair `tokio::time::pause()`/`advance()` with a real `SqlitePool` or
  `ControlPlane`.** Enforced by `just check-paused-time-db`. Neither identifier
  appears in the files this plan touches, so `start_paused` remains available
  there.
- **Zero warnings.** `just lint` is
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`, with
  `[workspace.lints]` pedantic on and `panic`/`unwrap`/`expect` denied in
  non-test code.
- **Error `code` strings are public contract.** Both new errors use the existing
  `VoomError::ExternalSystemUnavailable` variant; do not add a `VoomError`
  variant and do not change any `code()` mapping.
- **`voom-node-agent` has no `tracing` dependency** (only a dev-dependency on
  `tracing-subscriber`). Do not add one. The returned `VoomError` is the only
  operator channel.
- **Do not change `crates/voom-node-agent/tests/lifecycle.rs`'s `HANG_GUARD`**
  (30 s, `lifecycle.rs:47`). Charter exclusion, owner #446.
- **Do not touch `voom-control-plane` or `voom-store`.** Charter exclusion,
  owner #592.
- **Do not add or change any field in `AgentConfig`.** `shutdown_grace_seconds`
  keeps its `1..=60` validation (`config.rs:159-164`) unchanged.
- Guardrail commands: `just fmt`, `just lint`, `cargo test -p voom-node-agent`,
  and `just ci` before pushing. `just ci` runs `fmt-check`, `lint`,
  `check-test-layout`, the guard self-tests, `check-adr-index`, `test`, `doc`,
  `deny`, `audit`.
- Line length 100, functions ≤100 lines, cyclomatic complexity ≤8.

## File map

| File | Change |
|---|---|
| `crates/voom-node-agent/src/runtime.rs` | `ShutdownBudgets`, `ShutdownForce`, deadline parameters, `:315` mapping, readiness race, tail backstop, two new error constructors |
| `crates/voom-node-agent/src/runtime_test.rs` | New tests; update the sites that read or construct `ShutdownProgress.forced` and `LeaseSettlement::Forced` |
| `crates/voom-node-agent/src/child.rs` | `reap_after_kill` bound on the post-`start_kill` wait; `shutdown` and `shutdown_all` take it; test seam |
| `crates/voom-node-agent/src/child_test.rs` | Reap-bound test |
| `crates/voom-node-agent/tests/budget_ladder.rs` | The shutdown rung, recording the deliberate inversion |

No other file changes. `docs/adr/0088-…`, its `docs/adr/README.md` row, the design
spec, and `docs/runbooks/operator-node-agent.md` are already committed on this
branch.

---

## Task 1 — `ShutdownBudgets`

**Creates:** nothing. **Modifies:** `crates/voom-node-agent/src/runtime.rs`.
**Tests:** `crates/voom-node-agent/src/runtime_test.rs`.

Introduces the budget values and the runtime field that carries them. Nothing
consumes them yet; later tasks do. This task exists separately because every
later task needs the seam, and because a wrong `tail()` would be invisible once
buried inside a timeout.

### Interfaces

Provides to Tasks 2–6:

```rust
pub struct ShutdownBudgets { pub call: Duration, pub reap_after_kill: Duration, pub backstop_margin: Duration }
impl ShutdownBudgets { pub const DEFAULT: Self; pub fn tail(&self, grace: Duration) -> Duration; }
impl AgentRuntime { fn budgets(&self) -> ShutdownBudgets; }
```

Consumes: `AgentRuntime` and its existing `with_client` constructor
(`runtime.rs:124`).

### Steps

1. In `runtime.rs`, below the existing consts near line 42, add:

```rust
/// Wall-clock budgets for the shutdown tail.
///
/// Threaded as values rather than read from constants at the point of use: three
/// existing tests gate `deactivate` with a `Notify` that is never notified, and a
/// real 10 s timer read in place would make each of them a wall-clock race.
///
/// See `docs/adr/0088-bounded-node-agent-shutdown.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownBudgets {
    /// One control-plane wait in the shutdown tail.
    pub call: Duration,
    /// Collecting a killed child's exit status, after `SIGKILL`.
    pub reap_after_kill: Duration,
    /// Slack the tail backstop adds over the sum of the inner bounds.
    pub backstop_margin: Duration,
}

impl ShutdownBudgets {
    /// Production values. 10 s sits under the lifecycle suite's 30 s `HANG_GUARD`
    /// and under one `REQUEST_TIMEOUT`; 5 s of margin covers the tail costs that
    /// fall outside every inner bound.
    pub const DEFAULT: Self = Self {
        call: Duration::from_secs(10),
        reap_after_kill: Duration::from_secs(1),
        backstop_margin: Duration::from_secs(5),
    };

    /// Upper bound on the whole shutdown tail, and the number
    /// `docs/runbooks/operator-node-agent.md` publishes.
    #[must_use]
    pub fn tail(&self, grace: Duration) -> Duration {
        self.call
            .saturating_mul(2)
            .saturating_add(grace)
            .saturating_add(self.reap_after_kill)
            .saturating_add(self.backstop_margin)
    }
}
```

2. Add a `budgets: ShutdownBudgets` field to `pub struct AgentRuntime`
   (`runtime.rs:99`). Set it to `ShutdownBudgets::DEFAULT` in both `new`
   (`:113`) and `with_client` (`:124`).

2a. **Add `budgets: ShutdownBudgets` to `CoordinatorContext` too.** It is declared
   at `runtime.rs:639-658` (duration fields `lease_ttl`, `progress_timeout`,
   `poll_interval`, `shutdown_grace` at `:645-649`) and has no budget field today.
   Every later task that needs a budget inside a coordinator reads
   `context.budgets`, so without this they do not compile. Populate it in
   `spawn_coordinators` beside `shutdown_grace: self.shutdown_grace()` (`:487`)
   with `budgets: self.budgets()`, and in the `context()` test helper
   (`runtime_test.rs:2088-2104`) with `budgets: ShutdownBudgets::DEFAULT`. Do not
   read `ShutdownBudgets::DEFAULT` anywhere inside the coordinator: that would
   defeat the `#[cfg(test)]` override this task exists to provide and reintroduce
   the wall-clock races.

3. Add, beside `with_client`:

```rust
    #[cfg(test)]
    fn with_client_and_budgets(
        config: LoadedAgentConfig,
        client: Arc<dyn ControlPlaneApi>,
        budgets: ShutdownBudgets,
    ) -> Self {
        Self {
            budgets,
            ..Self::with_client(config, client)
        }
    }

    fn budgets(&self) -> ShutdownBudgets {
        self.budgets
    }
```

   `..Self::with_client(…)` requires `AgentRuntime`'s other fields to be
   constructible that way; if any field is not `Copy`/moveable in that position,
   write the struct literal out instead rather than deriving `Default`.

4. Add to `runtime_test.rs`:

```rust
#[test]
fn the_published_tail_bound_is_the_sum_of_every_inner_bound_plus_margin() {
    let budgets = ShutdownBudgets::DEFAULT;
    // The runbook tells operators to set the supervisor stop timeout above
    // `shutdown_grace_seconds + 26`. If this changes, that runbook is wrong.
    assert_eq!(budgets.tail(Duration::from_secs(10)), Duration::from_secs(36));
    // The validator caps shutdown_grace_seconds at 60 (config.rs:159-164), and the
    // worst case must stay inside systemd's upstream 90 s DefaultTimeoutStopSec.
    assert_eq!(budgets.tail(Duration::from_secs(60)), Duration::from_secs(86));
    assert!(budgets.tail(Duration::from_secs(60)) < Duration::from_secs(90));
}
```

5. Run `cargo test -p voom-node-agent the_published_tail_bound`. Expect
   `test result: ok. 1 passed`.
6. Run `just lint`. Under `-D warnings` an unread `budgets` field is a build
   failure, and nothing reads it until Task 2. **Do not add an `allow` and do not
   commit this task on its own** — run Task 2 and commit the two together as
   `feat(node-agent): add shutdown budgets and bound the reap`. That is the
   smallest commit that builds clean, and it is one logical change: the values
   and their first consumer.

### Acceptance criteria

- `ShutdownBudgets::DEFAULT.tail(60s) == 86s` and `< 90s`, asserted.
- `AgentRuntime` carries the budgets; a `#[cfg(test)]` constructor overrides them.
- No `AgentConfig` field added.

---

## Task 2 — bound the post-`SIGKILL` reap

**Modifies:** `crates/voom-node-agent/src/child.rs`,
`crates/voom-node-agent/src/child_test.rs`.

`RunningChild::shutdown` applies `grace` only to the polite wait at `child.rs:199`;
on expiry it calls `start_kill()` at `:204` and then `child.wait().await` at
`:207` with no timeout. A child the kernel cannot kill leaves that wait pending
forever, which is the hole under the arithmetic Task 1 asserted.

### Interfaces

Consumes from Task 1: nothing directly — the caller supplies the value.

Provides to Task 5:

```rust
impl RunningChild { async fn shutdown(&mut self, grace: Duration, reap_after_kill: Duration) -> Result<(), ChildError>; }
impl ChildSupervisor { pub fn new(shutdown_grace: Duration, reap_after_kill: Duration) -> Self; pub async fn shutdown_all(&self, children: Vec<RunningChild>) -> Result<(), ChildError>; }
```

### Steps

1. Write the failing test first, in `child_test.rs`. **It does not use a real
   child**, and that is deliberate: a real `child.wait()` immediately after
   `start_kill()` may or may not be ready on the first poll, so asserting a
   `Duration::ZERO` budget expires would race the kernel and flake. Test the
   bound where it lives instead, against a wait that is guaranteed never ready:

```rust
#[tokio::test(start_paused = true)]
async fn a_child_that_cannot_be_reaped_is_abandoned_at_the_bound() {
    // An unkillable process cannot be constructed portably, so this proves the
    // timeout is wired, not the uninterruptible-sleep case. It does not race the
    // kernel: the wait is `pending()`, so only the bound can resolve it.
    let error = reap_within(std::future::pending(), Duration::from_secs(1), "stubborn")
        .await
        .expect_err("an unreapable child must be abandoned, not waited on");
    assert!(
        error.to_string().contains("reap after kill"),
        "error must name the unreaped child: {error}"
    );
}

```

   The happy path needs no test of its own: `tokio::time::timeout` passing a ready
   future through is that crate's contract, and
   `shutdown_kills_and_reaps_a_child_that_ignores_stdin_eof` already covers it
   against a real process.

   The existing real-child coverage stays as it is:
   `shutdown_kills_and_reaps_a_child_that_ignores_stdin_eof`
   (`child_test.rs:348-366`) already exercises the polite-wait-expiry →
   `start_kill` → reap path end to end with `ChildFixture`,
   `ChildSupervisor::with_timeouts` and `assert_reaped`, and it must keep
   passing unchanged apart from the supervisor's new arity.

2. Run `cargo test -p voom-node-agent a_child_that_cannot_be_reaped`. Expect a
   compile error — `reap_within` does not exist. That is the red state.

3. Add the helper the test names, and have `shutdown` call it. Extracting it is
   what makes the bound testable without a process:

```rust
/// Collect a killed child's exit status, giving up after `bound`.
///
/// A `SIGKILL`ed child is already doomed and this wait only collects its status,
/// so abandoning it orphans nothing `SIGKILL` had not already claimed — `launch`
/// sets `.kill_on_drop(true)`, and init reaps the reparented process. Without the
/// bound, a child in uninterruptible sleep pends here forever and takes the whole
/// shutdown tail with it.
async fn reap_within<F>(wait: F, bound: Duration, name: &str) -> Result<(), ChildError>
where
    F: Future<Output = std::io::Result<std::process::ExitStatus>>,
{
    match tokio::time::timeout(bound, wait).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(ChildError::shutdown(name, format!("reap after kill: {error}"))),
        Err(_) => Err(ChildError::shutdown(
            name,
            format!("reap after kill: not reaped within {bound:?}"),
        )),
    }
}
```

4. Change `RunningChild::shutdown` (`child.rs:193`) to take the second argument
   and call it:

```rust
    async fn shutdown(
        &mut self,
        grace: Duration,
        reap_after_kill: Duration,
    ) -> Result<(), ChildError> {
        drop(self.stdin.take());
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        let name = self.spec.logical_name.clone();
        if let Ok(result) = tokio::time::timeout(grace, child.wait()).await {
            result.map_err(|error| {
                ChildError::shutdown(&name, format!("wait after stdin EOF: {error}"))
            })?;
        } else {
            child.start_kill().map_err(|error| {
                ChildError::shutdown(&name, format!("kill after shutdown timeout: {error}"))
            })?;
            reap_within(child.wait(), reap_after_kill, &name).await?;
        }
        self.reaped = true;
        Ok(())
    }
```

5. Add `reap_after_kill: Duration` to `ChildSupervisor` (declared `child.rs:256`),
   to `pub fn new` (`:264`) and to `fn with_timeouts` (declared `:276`, behind the
   `#[cfg(all(test, target_os = "linux"))]` attribute at `:274`), and pass it at
   the `child.shutdown(grace).await` call inside `shutdown_all` (`:360`):
   `child.shutdown(grace, reap).await`, where `reap` is captured beside `grace`
   the same way. Update `with_timeouts`' existing caller in `child_test.rs`
   (`:353` and the other startup tests) to the new arity.

6. Fix every `ChildSupervisor::new` call site. There are **four**:
   `runtime.rs:224`, `:745`, `:795` and `:852` (inside `restart_child`) —
   `grep -n 'ChildSupervisor::new' crates/voom-node-agent/src/*.rs` is the census,
   and missing `:852` is the easy mistake because the other three cluster. Pass
   `self.budgets().reap_after_kill` at `:224`, and `context.reap_after_kill` at
   the other three, reading `context.budgets.reap_after_kill` — the field Task 1
   step 2a added. No new `CoordinatorContext` field is needed here.

7. Run `cargo test -p voom-node-agent reaped`. Expect both new tests passing.
8. Run `cargo test -p voom-node-agent` and `just lint`. Expect all green, with
   `shutdown_kills_and_reaps_a_child_that_ignores_stdin_eof` still passing.
9. Commit Tasks 1 and 2 together:
   `feat(node-agent): add shutdown budgets and bound the reap`.

### Acceptance criteria

- `reap_within` against a never-ready wait returns a `ChildError` naming the
  child, deterministically, with no process involved.
- The polite-wait path and its error text are unchanged.
- No call site of `ChildSupervisor::new` is left on the old arity.

---

## Task 3 — bound the settlement wait and carry its cause

**Modifies:** `crates/voom-node-agent/src/runtime.rs`,
`crates/voom-node-agent/src/runtime_test.rs`.

`wait_or_force` is where the settlement wait blocks. It gets an optional
deadline, supplied by the two call sites that only run during a shutdown and
withheld from the one that runs in steady state.

### Interfaces

Consumes from Task 1: `AgentRuntime::budgets()`, `ShutdownBudgets::call`.

Provides to Task 4 and Task 5:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShutdownForce { Signal, Deadline }

async fn wait_or_force(
    leases: &mut JoinSet<()>,
    shutdown: &mut watch::Receiver<ShutdownKind>,
    report_any: bool,
    deadline: Option<Instant>,
) -> Option<(ShutdownKind, Option<ShutdownForce>)>;

pub(crate) enum LeaseSettlement { Completed, Forced(ShutdownForce) }
struct ShutdownProgress { signal_phase: ShutdownSignalPhase, forced: Option<ShutdownForce> }

fn shutdown_deadline_error() -> VoomError;
```

### Steps

1. Add to `runtime.rs`, beside `forced_shutdown_error` (`:2075`):

```rust
/// A shutdown-tail control-plane wait was abandoned at its own budget.
///
/// Distinct from `forced_shutdown_error` because no signal arrived: an operator
/// told to look for one would be looking for something nobody sent.
fn shutdown_deadline_error() -> VoomError {
    VoomError::ExternalSystemUnavailable(
        "node-agent shutdown abandoned a control-plane call at its shutdown budget; \
         the control plane did not answer"
            .to_owned(),
    )
}
```

2. Add `ShutdownForce` beside `LeaseSettlement` (`:1918`), change
   `LeaseSettlement::Forced` to `Forced(ShutdownForce)`, and change
   `ShutdownProgress.forced` from `bool` to `Option<ShutdownForce>`.

3. Change `wait_or_force` (`:1744`) to take `deadline: Option<Instant>` and
   return `Option<(ShutdownKind, Option<ShutdownForce>)>`. Inside the existing
   `loop`, add a third `select!` arm, and tag the two existing break paths:

```rust
                () = async {
                    match deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        // No deadline: never ready, so the arm never fires.
                        None => std::future::pending().await,
                    }
                } => break Some((ShutdownKind::Forced, Some(ShutdownForce::Deadline))),
```

   The watch-closed break and the `kind == Forced` break both carry
   `Some(ShutdownForce::Signal)`; the `report_any` break carries `None`, because
   it reports an observation without having forced anything —
   `child_crash_lease_settlement` maps that to `Completed`. Keep the existing
   `if observed == Some(ShutdownKind::Forced) { leases.abort_all(); … }` post-step,
   matching on the tuple's first element.

4. Supply the deadline at two of the three call sites. `Instant` throughout is
   `tokio::time::Instant`, so a paused clock advances it.

   - **`:1798`, inside `settle_leases_for_shutdown`.** Add
     `deadline: Option<Instant>` to that function's signature
     (declared `runtime.rs:1778-1783`). Its only caller is `run_coordinator`'s
     `CoordinatorEvent::Shutdown` arm (`:743-744`), which has `context` in scope
     and passes `Some(Instant::now() + context.budgets.call)`.
   - **`:1718`, inside `settle_leases_after_child_crash`.** This looks like a
     crash-path wait and is not one: `let observed = observed?;` at `:1714`
     returns unless a shutdown was already observed, so `:1718` runs only with a
     shutdown already in flight. Give
     `settle_leases_after_child_crash` (declared `:1708-1712`) a
     **`budget: Duration`** parameter, not an `Instant`, and build the deadline
     *inside* it, immediately after the `observed?` short-circuit:
     `let deadline = Some(Instant::now() + budget);`. Its caller
     `restart_after_child_exit` (`:772`) passes `context.budgets.call`.

     **Passing an `Instant` computed in the caller would be a bug**, and a quiet
     one. `settle_leases_after_child_crash` runs `cancel_and_wait` first
     (`:1713`), which is the wait this design deliberately leaves unbounded and
     which can sit for up to `production_request_budget()` = 153.75 s. A deadline
     stamped before it arrives at `:1718` already expired, so the second wait
     forces instantly — aborting leases that may have been one round-trip from
     settling, recording `Forced(Deadline)`, and exiting non-zero. The budget
     there would not be shortened; it would be zero. Every other deadline site in
     this plan (`:743`, `:230`, `:249`, `:319`) computes immediately before its
     own call, which is why they are correct as written.
   - **`:1705`, inside `cancel_and_wait`.** Pass `None` — literally, at the call
     site inside `cancel_and_wait` (declared `:1698-1703`). `cancel_and_wait`
     gains **no** parameter: it has exactly one caller
     (`settle_leases_after_child_crash:1713`) and that caller always wants the
     unarmed behaviour. This is the only genuinely steady-state site; arming it
     would terminate a healthy running agent after one slow worker-crash
     settlement.

4a. **Rewrite the two `LeaseSettlement::Forced` constructions in
   `settle_leases_for_shutdown` — this is the hop that carries `Deadline` out of
   the coordinator, and it is the one place the whole feature can be silently
   nullified.** The function body (`:1784-1803`) has both:

```rust
    if kind == ShutdownKind::Forced {
        leases.abort_all();
        wait_for_leases(leases).await;
        // A published Forced kind only ever comes from wait_for_coordinators'
        // signal arm, so this force is a signal.
        return LeaseSettlement::Forced(ShutdownForce::Signal);
    }
    match wait_or_force(leases, shutdown, false, deadline).await {
        Some((ShutdownKind::Forced, cause)) => {
            LeaseSettlement::Forced(cause.unwrap_or(ShutdownForce::Signal))
        }
        _ => LeaseSettlement::Completed,
    }
```

   The old code was `if wait_or_force(...).await == Some(ShutdownKind::Forced)`,
   which stops typechecking against the new tuple return. **The obvious way to
   satisfy the compiler is `LeaseSettlement::Forced(ShutdownForce::Signal)` at
   both sites, and that would compile, pass every test in this task, and make the
   feature a no-op** — `ShutdownProgress.forced` could never hold `Deadline`, the
   new arm in `finish_shutdown_lifecycle` would be unreachable, and a settlement
   abandoned at its budget would report a signal force and skip the deactivation.
   Task 3's second-to-last test exists to catch exactly that, and it must obtain
   its `ShutdownProgress` from `wait_for_coordinators` rather than constructing
   one.

5. In `finish_shutdown_lifecycle` (`:298`), replace
   `if progress.forced { return Err(forced_shutdown_error()); }` at `:315` with:

```rust
        // A signal force is the operator saying stop, and skips the write as it always
        // has. A deadline expiring is not that instruction: the write is what this
        // whole bound exists to protect, and attempting it costs one more budget.
        match progress.forced {
            Some(ShutdownForce::Signal) => return Err(forced_shutdown_error()),
            Some(ShutdownForce::Deadline) | None => {}
        }
```

   and, after the existing `deactivate_or_second_signal(…).await?` at `:319`,
   before the `match exit`:

```rust
        if progress.forced == Some(ShutdownForce::Deadline) {
            return Err(shutdown_deadline_error());
        }
```

6. Fix the remaining mechanical fallout the compiler names.
   `child_crash_lease_settlement` (`:1722-1731`) takes
   `final_observed: Option<(ShutdownKind, Option<ShutdownForce>)>` and constructs
   `LeaseSettlement::Forced(ShutdownForce::Deadline)` when that tuple's cause is
   `Deadline` — `:1718` **is** armed — and `Forced(ShutdownForce::Signal)`
   otherwise;
   `wait_for_coordinators`' join arm (`:1822`) becomes
   `Some(Ok(CoordinatorExit::Shutdown(LeaseSettlement::Forced(cause)))) => { forced.get_or_insert(cause); }`;
   its signal arm (`:1838`) guard `!forced` becomes `forced.is_none()` and its
   body sets `forced = Some(ShutdownForce::Signal)`; `runtime_test.rs:107`,
   `:141`, `:969`, `:1051`, `:1136` (read/construct `ShutdownProgress.forced`)
   and `:921`, `:947`, `:955`, `:1002` (construct `LeaseSettlement::Forced`)
   take the payload.

7. Add to `runtime_test.rs`:

```rust
#[tokio::test(start_paused = true)]
async fn shutdown_deadline_forces_blocked_lease_settlement() {
    let (_cancel_tx, mut leases, _shutdown_tx, mut shutdown_rx) = never_settling_leases();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let observed = wait_or_force(&mut leases, &mut shutdown_rx, false, Some(deadline)).await;
    assert_eq!(
        observed,
        Some((ShutdownKind::Forced, Some(ShutdownForce::Deadline))),
        "an overrunning settlement must be abandoned at its own budget, and say so"
    );
}

#[tokio::test(start_paused = true)]
async fn the_crash_settlement_budget_starts_when_its_own_wait_does() {
    // Regression for the deadline-stamped-too-early bug: the first wait is unbounded,
    // so a budget computed before it would arrive at the second wait already expired.
    let (cancel_tx, mut leases, shutdown_tx, mut shutdown_rx) =
        leases_settling_after(Duration::from_secs(30));
    shutdown_tx.send(ShutdownKind::User).unwrap();
    let settlement = settle_leases_after_child_crash(
        &cancel_tx,
        &mut leases,
        &mut shutdown_rx,
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(
        settlement,
        Some(LeaseSettlement::Completed),
        "a 60s budget must still be 60s when the second wait starts, not zero"
    );
}

#[tokio::test(start_paused = true)]
async fn the_crash_settlement_path_is_not_armed() {
    // wait_or_force with no deadline must wait, whatever the clock does: arming the
    // crash path would terminate a healthy running agent after one slow settlement.
    let (_cancel_tx, mut leases, _shutdown_tx, mut shutdown_rx) =
        leases_settling_after(Duration::from_secs(60));
    let observed = wait_or_force(&mut leases, &mut shutdown_rx, false, None).await;
    assert_eq!(observed, None, "an unarmed wait must settle, not force");
}

#[tokio::test(start_paused = true)]
async fn a_deadline_forced_settlement_reports_deadline() {
    // Obtained from the code under test, not hand-constructed: the whole feature is
    // nullified if settle_leases_for_shutdown collapses every force to Signal, and a
    // hand-built ShutdownProgress cannot see that.
    let (cancel_tx, mut leases, _shutdown_tx, mut shutdown_rx) = never_settling_leases();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let settlement = settle_leases_for_shutdown(
        &cancel_tx,
        &mut leases,
        &mut shutdown_rx,
        ShutdownKind::User,
        Some(deadline),
    )
    .await;
    assert_eq!(
        settlement,
        LeaseSettlement::Forced(ShutdownForce::Deadline),
        "a settlement abandoned at its budget must not report itself as a signal force"
    );
}

#[tokio::test(start_paused = true)]
async fn a_deadline_forced_settlement_still_deactivates() {
    let control = Arc::new(FakeControlPlane::default());
    let runtime = AgentRuntime::with_client(loaded_config(), control.clone());
    let (_signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    let error = runtime
        .finish_shutdown_lifecycle(
            incarnation(),
            RuntimeExit::Graceful,
            Ok(ShutdownProgress {
                signal_phase: ShutdownSignalPhase::ForceEnabled,
                forced: Some(ShutdownForce::Deadline),
            }),
            pending_heartbeat_handle(),
            &mut signal_rx,
        )
        .await
        .expect_err("a deadline force still exits unsuccessfully");
    assert!(
        error.to_string().contains("shutdown budget"),
        "must name the budget, not a signal nobody sent: {error}"
    );
    assert_eq!(
        control.events.lock().await.as_slice(),
        &["deactivate"],
        "the Retired write is what the bound exists to protect"
    );
}
```

   **Both helpers return the same four-tuple**, so every call site destructures
   identically:

```rust
fn never_settling_leases()
    -> (watch::Sender<LeaseCancellation>, JoinSet<()>, watch::Sender<ShutdownKind>, watch::Receiver<ShutdownKind>);
fn leases_settling_after(after: Duration)
    -> (watch::Sender<LeaseCancellation>, JoinSet<()>, watch::Sender<ShutdownKind>, watch::Receiver<ShutdownKind>);
```

   The `Sender` is returned as well as the `Receiver` because the tests need to
   publish a kind and to keep the channel open. `never_settling_leases` and
   `leases_settling_after` are new local helpers.
   Build them from the lease-`JoinSet` setup the existing
   `settle_leases_for_shutdown` tests already use — the ones around
   `runtime_test.rs:920-1010` that construct a `JoinSet<()>`, a
   `watch::channel(LeaseCancellation::Running)` and a
   `watch::channel(ShutdownKind::Running)` by hand. `never_settling_leases`
   spawns one task that awaits `std::future::pending()`;
   `leases_settling_after(d)` spawns one that sleeps `d`.

7a. Add the R2 regression, in `runtime_test.rs`. Nothing existing covers it:
   `loaded_config` sets `shutdown_grace_seconds: 1` (`runtime_test.rs:2076`) and
   `context()` sets `shutdown_grace: Duration::from_secs(1)` (`:2103`), so no test
   has a reap that outlasts a call budget. This is the hazard the ADR's rejection
   of the `wait_for_coordinators` placement turns on — "a unit in `failed` on
   every `systemctl stop`" — and without it nothing catches a regression back to
   it:

```rust
#[tokio::test]
#[cfg(unix)]
async fn a_full_shutdown_grace_is_not_a_forced_shutdown() {
    // A worker that uses its whole grace, against a healthy control plane. The reap
    // outlasts `budgets.call`, and that must not mark the shutdown forced: the
    // incarnation retires and the process exits 0. Real time and a real child, so
    // the budgets are milliseconds and the grace is longer than `call`.
    let budgets = ShutdownBudgets {
        call: Duration::from_millis(50),
        reap_after_kill: Duration::from_millis(50),
        backstop_margin: Duration::from_secs(30),
    };
    // … a worker that ignores stdin EOF, and a config whose shutdown_grace_seconds
    // exceeds 50ms — ProcessWorkerFixture, as
    // `graceful_shutdown_settles_before_child_reap_and_deactivation` (:746) builds it …
    let result = runtime.run_until(async { let _ = stop_rx.await; }).await;
    assert!(result.is_ok(), "a routine stop must not exit non-zero: {result:?}");
    assert!(
        control.events.lock().await.contains(&"deactivate".to_owned()),
        "and must still retire the incarnation"
    );
}
```

   `backstop_margin` is deliberately large here: the backstop must not fire, so
   that a failure points at the settlement placement rather than at the backstop.

8. Run `cargo test -p voom-node-agent`. Expect all green.
9. Run `just lint`. Expect exit 0.
10. Commit: `fix(node-agent): bound the shutdown settlement wait`.

### Acceptance criteria

- A settlement overrunning `budgets.call` is abandoned and reported as
  `Deadline` **by `settle_leases_for_shutdown` itself**, and still reaches the
  deactivation.
- `wait_or_force` with `deadline: None` never forces on time alone.
- A signal force still returns `forced_shutdown_error()` before deactivating.

---

## Task 4 — bound the deactivation wait

**Modifies:** `crates/voom-node-agent/src/runtime.rs`,
`crates/voom-node-agent/src/runtime_test.rs`.

### Interfaces

Consumes from Task 3: `shutdown_deadline_error()`.

Provides:

```rust
impl AgentRuntime {
    async fn deactivate_or_second_signal(
        &self,
        incarnation_id: NodeIncarnationId,
        reason: NodeIncarnationEndReason,
        signals: &mut mpsc::UnboundedReceiver<()>,
        signal_phase: &mut ShutdownSignalPhase,
        deadline: Instant,
    ) -> Result<(), VoomError>;
}
```

### Steps

1. Add the `deadline: Instant` parameter to `deactivate_or_second_signal`
   (`runtime.rs:539`) and a third `select!` arm inside its `loop`:

```rust
                () = tokio::time::sleep_until(deadline) => {
                    return Err(shutdown_deadline_error());
                }
```

   A parameter rather than a constant read in place: several existing tests are
   plain `#[tokio::test]` on real time and gate `deactivate` with a `Notify` that
   is never notified, so a real 10 s timer read here would make each a wall-clock
   race.

2. Pass `Instant::now() + self.budgets().call` at all three call sites —
   `:230`, `:249` and `:319`. The first two are startup-failure paths that run
   before any shutdown tail exists, so each computes its own.

3. Fix the existing tests. `rg -n 'deactivate_or_second_signal' crates/voom-node-agent/src/runtime_test.rs`
   is the census, and it returns **two** direct call sites, not three. They split
   into two groups needing two different remedies:

   **Direct callers — give them a far-future deadline argument**
   (`tokio::time::Instant::now() + Duration::from_secs(3_600)`; they test the
   signal, not the budget):
   - `:1019`, in `second_signal_interrupts_deactivation_only_after_reap` (`:1009`);
   - `:1228`, in `second_signal_interrupts_a_non_graceful_deactivation` (`:1218`).
     This one is easy to miss — it is not adjacent to the other — and it will
     fail to compile on the new arity.

   **Indirect callers — give them far-future *budgets* instead.** These reach
   deactivation through a function that computes the deadline internally, so
   there is no argument to pass:
   - `restart_exhausted_deactivation_requires_a_genuine_second_signal` (`:1040`)
     calls `runtime.finish_shutdown_lifecycle(...)` at `:1045`;
   - `child_startup_failure_deactivation_requires_a_genuine_second_signal`
     (`:1085`) calls `runtime.run_with_shutdowns(signal_rx)` at `:1090`, reaching
     the `:230` startup-failure deactivation.

     Build both runtimes with `AgentRuntime::with_client_and_budgets(...)` and a
     `call` of `Duration::from_secs(3_600)`. That is what Task 1's seam is for.
     Left alone they would inherit a real 10 s timer and race their own
     `timeout(Duration::from_millis(50), ...)` assertions (`:1101-1106`) — the
     exact wall-clock race the parameter exists to avoid.

4. Add to `runtime_test.rs`:

```rust
#[tokio::test(start_paused = true)]
async fn shutdown_deadline_abandons_a_blocked_deactivation() {
    let control = Arc::new(FakeControlPlane::default());
    *control.deactivate_gate.lock().await = Some(Arc::new(Notify::new()));
    let runtime = AgentRuntime::with_client(loaded_config(), control.clone());
    let (_signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    let mut signal_phase = ShutdownSignalPhase::ForceEnabled;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

    // The outer timeout is what makes this a regression rather than a hang: before
    // the change the inner call never returns and this fails on Elapsed.
    let error = tokio::time::timeout(
        Duration::from_secs(600),
        runtime.deactivate_or_second_signal(
            incarnation(),
            NodeIncarnationEndReason::GracefulShutdown,
            &mut signal_rx,
            &mut signal_phase,
            deadline,
        ),
    )
    .await
    .expect("a blocked deactivation must not outlive its budget")
    .expect_err("and must report the budget");
    assert!(error.to_string().contains("shutdown budget"), "{error}");
}
```

5. Run `cargo test -p voom-node-agent shutdown_deadline_abandons`. Expect
   `test result: ok. 1 passed`.
6. Run `cargo test -p voom-node-agent` and `just lint`. Expect green.
7. Commit: `fix(node-agent): bound the shutdown deactivation wait`.

### Acceptance criteria

- A gated `deactivate` yields `shutdown_deadline_error()` at the budget.
- The three existing second-signal tests still assert the signal as the cause.

---

## Task 5 — release the coordinator waits that ignore the shutdown

**Modifies:** `crates/voom-node-agent/src/runtime.rs`,
`crates/voom-node-agent/src/runtime_test.rs`.

`restart_after_child_exit` (`runtime.rs:772-819`) awaits `set_worker_readiness`
at `:781-790` and again at `:807-815`, and `restart_child` at `:804-806`, with no
`select!` against the shutdown receiver. A coordinator blocked there is not
released by anything, so `wait_for_coordinators` waits out the client's 153.75 s
retry budget — or, for an accelerator worker, up to three
`NVIDIA_STARTUP_TIMEOUT`s of five minutes (`child.rs:22-24`).

### Steps

1. Add a helper beside `restart_after_child_exit`:

```rust
/// Await `work`, abandoning it once a shutdown is in flight.
///
/// The predicate runs on a **clone** of the receiver. `changed()` on the original
/// would mark the value seen, and the value is sent exactly once
/// (`run_with_seeded_shutdowns`); `settle_leases_after_child_crash`'s
/// `cancel_and_wait` wait is deliberately unbounded and `shutdown.changed()` is its
/// only escape, so consuming the notification here would strand it. A clone tracks
/// its own seen version, and `wait_for` checks the current value first.
async fn until_shutdown<F, T>(
    shutdown: &watch::Receiver<ShutdownKind>,
    work: F,
) -> Option<T>
where
    F: Future<Output = T>,
{
    let mut watcher = shutdown.clone();
    tokio::select! {
        value = work => Some(value),
        _ = watcher.wait_for(|kind| *kind != ShutdownKind::Running) => None,
    }
}
```

2. Wrap the `NotReady` readiness call at `:781`:

```rust
    let readiness = until_shutdown(
        shutdown,
        set_worker_readiness(
            context.client.as_ref(),
            context.node_id,
            context.incarnation_id,
            context.worker_id,
            WorkerReadiness::NotReady,
        ),
    )
    .await;
    if let Some(Err(error)) = readiness {
        return Err(CoordinatorExit::Fatal(RuntimeFatal::ControlPlane(error)));
    }
```

   A `None` falls through to the existing `settle_leases_after_child_crash` call
   immediately below, which is where a coordinator that has observed a shutdown
   is already meant to go. Do not add an early return: the comment at `:791-793`
   records why settlement must run and be returned.

3. Wrap the `restart_child` call at `:804` and the `Ready` readiness call at
   `:807` the same way. **Both new early returns must reap the child first**,
   mirroring the existing shutdown branch immediately above (`:794-798`):

```rust
        let supervisor = ChildSupervisor::new(context.shutdown_grace, context.budgets.reap_after_kill);
        let _ = supervisor.shutdown_all(vec![child]).await;
        return Err(CoordinatorExit::Shutdown(LeaseSettlement::Completed));
```

   Without it the child falls to `RunningChild::Drop` (`child.rs:216-231`), which
   `start_kill`s it — and at `:807` the child is `restarted`, freshly launched and
   alive, so it would be `SIGKILL`ed instead of getting its stdin closed and its
   `shutdown_grace_seconds` honoured. Criterion R2 preserves the
   settlement → child-reaping → deactivation ordering, and this is that reaping
   step on a newly reachable path. At `:807` the binding to reap is `restarted`,
   not the original `child`, which has already been consumed by `restart_child`.

4. Add to `runtime_test.rs`:

```rust
#[tokio::test]
#[cfg(unix)]
async fn a_crashed_worker_release_does_not_wait_out_the_retry_budget() {
    // Real time and a real child: restart_after_child_exit takes a RunningChild by
    // value. Budgets are shrunk to milliseconds so this costs well under a second.
    let control = Arc::new(FakeControlPlane::default());
    *control.readiness_gate.lock().await = Some(Arc::new(Notify::new()));
    // … build the coordinator context and a crashed child through ProcessWorkerFixture,
    // as `child_crash_restarts_only_after_every_held_lease_settles` does …
    shutdown_tx.send(ShutdownKind::User).unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(5), restarting)
        .await
        .expect("a shutdown must release the readiness call, not drain the retry budget")
        .unwrap();
    assert!(matches!(exit, Err(CoordinatorExit::Shutdown(_))));
}
```

   `readiness_gate` is a new `Mutex<Option<Arc<Notify>>>` on `FakeControlPlane`,
   modelled exactly on the existing `deactivate_gate`, awaited at the top of the
   fake's `worker_readiness`.

5. Run `cargo test -p voom-node-agent a_crashed_worker_release`. Expect
   `test result: ok. 1 passed`, in under 5 s.
6. Run `cargo test -p voom-node-agent` and `just lint`. Expect green.
7. Commit: `fix(node-agent): release the crash-restart waits on shutdown`.

### Acceptance criteria

- A shutdown observed while a readiness call is in flight releases it.
- `cancel_and_wait`'s wait still observes the shutdown afterwards — the existing
  crash-settlement tests still pass unmodified.
- No steady-state behaviour changes: with no shutdown, the predicate never holds.

---

## Task 6 — the tail backstop and the ladder rung

**Modifies:** `crates/voom-node-agent/src/runtime.rs`,
`crates/voom-node-agent/src/runtime_test.rs`,
`crates/voom-node-agent/tests/budget_ladder.rs`.

Tasks 3–5 bound the waits this design enumerated. The backstop bounds the ones it
did not.

### Steps

1. Add beside `shutdown_deadline_error`:

```rust
/// The whole shutdown tail overran its published bound.
///
/// Distinct from `shutdown_deadline_error` on purpose: that one is an ordinary,
/// documented abandon at a named budget. This one means a wait nobody bounded ran
/// past every inner budget, which is a defect and should be reported as one.
fn shutdown_backstop_error(bound: Duration) -> VoomError {
    VoomError::ExternalSystemUnavailable(format!(
        "node-agent shutdown exceeded its {bound:?} tail bound; a wait outside the \
         shutdown budgets did not complete — please report this"
    ))
}
```

2. Extract the wrapper so the backstop is testable without fault injection. Once
   Tasks 3-5 land, no *reachable* wait can outlast every inner bound — which is
   the point, and which means a test cannot park a real coordinator anywhere the
   backstop would fire. Test the wrapper directly instead:

```rust
/// Run the shutdown tail, giving up after `bound`.
///
/// Dropping `tail` drops the coordinator `JoinSet`, aborting its tasks. A child
/// mid-reap is still `SIGKILL`ed, by `.kill_on_drop(true)` in `launch`
/// (`child.rs:404`) — not by `RunningChild::Drop`, which early-returns once
/// `shutdown` has moved the handle out at `child.rs:195`.
async fn run_shutdown_tail_within<F>(bound: Duration, tail: F) -> Result<(), VoomError>
where
    F: Future<Output = Result<(), VoomError>>,
{
    match tokio::time::timeout(bound, tail).await {
        Ok(result) => result,
        Err(_) => Err(shutdown_backstop_error(bound)),
    }
}
```

3. In `run_with_seeded_shutdowns` (`:202`), wrap the tail. Replace the existing

```rust
        let settled =
            wait_for_coordinators(&mut coordinators, &shutdown_tx, &mut signals, signal_phase)
                .await;
        self.finish_shutdown_lifecycle(incarnation_id, exit, settled, node_heartbeat, &mut signals)
            .await
```

   with

```rust
        let bound = self.budgets().tail(self.shutdown_grace());
        let tail = async {
            let settled =
                wait_for_coordinators(&mut coordinators, &shutdown_tx, &mut signals, signal_phase)
                    .await;
            self.finish_shutdown_lifecycle(
                incarnation_id,
                exit,
                settled,
                node_heartbeat,
                &mut signals,
            )
            .await
        };
        run_shutdown_tail_within(bound, tail).await
```

   `exit` and `node_heartbeat` move into the async block, so the borrow checker
   requires them to be constructed before it; they already are.

4. Add to `runtime_test.rs`. Only the firing case is worth a test; the
   pass-through case is `tokio::time::timeout`'s own contract:

```rust
#[tokio::test(start_paused = true)]
async fn a_second_signal_after_a_deadline_force_still_forces() {
    // Criterion 4 is the one criterion whose semantics this change narrows — ratified
    // as unchanged in outcome, explicitly changed in latency — so the new ordering
    // gets its own test. A Deadline force is recorded first; the operator's second
    // signal then arrives, and the run must still end in forced_shutdown_error() with
    // the write skipped. Only the latency moved.
    let (_cancel_tx, mut coordinators, shutdown_tx, _shutdown_rx) = coordinators_forcing_on_deadline();
    let (signal_tx, mut signals) = mpsc::unbounded_channel();
    signal_tx.send(()).unwrap();
    let progress = wait_for_coordinators(
        &mut coordinators,
        &shutdown_tx,
        &mut signals,
        ShutdownSignalPhase::ForceEnabled,
    )
    .await
    .unwrap();
    // First force recorded wins — the signal arm's existing guard is unchanged — so the
    // cause stays Deadline and finish_shutdown_lifecycle still reaches deactivation.
    assert_eq!(progress.forced, Some(ShutdownForce::Deadline));
}

#[tokio::test(start_paused = true)]
async fn the_tail_backstop_bounds_a_wait_no_inner_budget_covers() {
    // `pending()` stands in for a wait nobody raced. After Tasks 3-5 no reachable
    // wait is like that — which is the design working — so the backstop is tested
    // at the function that implements it rather than through fault injection.
    let error = run_shutdown_tail_within(Duration::from_secs(30), std::future::pending())
        .await
        .expect_err("the backstop must fire");
    assert!(
        error.to_string().contains("please report this"),
        "the backstop must name itself as a defect, not reuse the deadline message: {error}"
    );
}

```

5. Add the rung to `crates/voom-node-agent/tests/budget_ladder.rs`. Extend the
   module doc's ladder diagram with a line recording the inversion, then:

```rust
#[test]
fn the_shutdown_budgets_deliberately_invert_the_ladder() {
    // Every other rung in this file asserts that an observer outlasts what it
    // observes. The shutdown budgets do the opposite, and that is the decision:
    // during shutdown the agent's obligation is to exit, and the failure underneath
    // is one it can no longer act on. See ADR 0088.
    assert!(
        ShutdownBudgets::DEFAULT.call < production_request_budget(),
        "the shutdown budget is meant to cut a retrying call short"
    );
}

```

   Import `ShutdownBudgets` as `use voom_node_agent::runtime::ShutdownBudgets;`,
   beside the existing `voom_node_agent::client` import. The 90 s ceiling is
   asserted once, in Task 1's `runtime_test.rs` test; do not repeat it here.
   `budget_ladder.rs` owns the ordering relationship, `runtime_test.rs` owns the
   magnitudes.

6. Run `cargo test -p voom-node-agent --test budget_ladder`. Expect all tests
   passing.
7. Run `cargo test -p voom-node-agent` and `just lint`. Expect green.
8. Commit: `fix(node-agent): back the shutdown tail with a bound`.

### Acceptance criteria

- `run_shutdown_tail_within` returns `shutdown_backstop_error` on a tail that
  never finishes, and passes a finished tail's result through unchanged.
- The backstop's message is distinguishable from the deadline's.
- `budget_ladder.rs` records the inversion **only**. The 90 s ceiling is asserted
  once, in Task 1's `runtime_test.rs` test — `budget_ladder.rs` owns the ordering
  relationship, `runtime_test.rs` owns the magnitudes.
- The deadline-then-signal ordering criterion 4 was narrowed to has a test.

---

## Task 7 — the end-to-end regression, and what actually discharges R5

**Modifies:** `crates/voom-node-agent/src/runtime_test.rs`.

R5 asks for "a deterministic regression that fails against the pre-change
behaviour". Most of the tests above cannot supply it: they name
`ShutdownBudgets`, `ShutdownForce`, `reap_within`, `run_shutdown_tail_within` or a
new parameter, so against the pre-change tree they do not *fail*, they do not
*build*. That is real coverage of the new behaviour and it is not the same
evidence.

**Two tests discharge R5**, and neither is
`shutdown_deadline_abandons_a_blocked_deactivation` — Task 4 gives
`deactivate_or_second_signal` a `deadline` parameter and that test passes it, so
pre-change it does not build.

The first is this task's
`a_sigterm_exits_the_agent_when_the_control_plane_never_answers`, and it is the
one that covers the charter's headline behaviour. It must therefore be built from
**pre-existing names only**: `AgentRuntime::with_client`, `run_until`, and the
existing `deactivate_gate` — **not** `with_client_and_budgets`, which this change
introduces. That means it takes the default budgets and costs about 10 s of real
wall clock. That cost is the price of having a witness at all, and the spec's
wall-clock section is corrected to predict it.

The second is Task 5's `a_crashed_worker_release_does_not_wait_out_the_retry_budget`.
It touches no new production name: it drives `restart_after_child_exit` with its existing
signature, and its only new machinery is a `readiness_gate` on
`FakeControlPlane`, which is test-support and compiles against the pre-change
tree. Pre-change, the gated readiness call drains the client's retry budget and
the outer `timeout(Duration::from_secs(5), …)` fails on `Elapsed`. Post-change
the shutdown releases it. **Verify this claim rather than asserting it:** before
committing Task 5, `git stash` the production change, run the test, and confirm
it fails on `Elapsed`; then restore and confirm it passes. Record both results in
the commit message.

### Steps

1. Add the end-to-end test. It uses the Task 1 seam, so it is coverage rather
   than R5 evidence, and it is what a reader looks for when asking whether the
   charter's own scenario is covered:

```rust
#[tokio::test]
#[cfg(unix)]
async fn a_sigterm_exits_the_agent_when_the_control_plane_never_answers() {
    // The charter's scenario end to end: one shutdown request, a control plane that
    // does not answer the deactivation, and an agent that must still exit.
    //
    // Deliberately built from pre-existing names only — with_client, run_until, and
    // deactivate_gate — so it compiles against the pre-change tree and fails there on
    // Elapsed. That is what makes it R5 evidence rather than coverage, and it is why
    // it takes the default budgets and costs about 10 s instead of milliseconds.
    let control = Arc::new(FakeControlPlane::default());
    *control.deactivate_gate.lock().await = Some(Arc::new(Notify::new()));
    let runtime = AgentRuntime::with_client(
        loaded_config_with_worker(fixture_worker()),
        control.clone(),
    );
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(async move { runtime.run_until(async { let _ = stop_rx.await; }).await });
    // … wait for readiness the way ProcessWorkerFixture-based tests do …
    stop_tx.send(()).unwrap();

    // Well above the 86 s tail, so pre-change this fails on Elapsed rather than
    // hanging the suite, and post-change it returns at about 10 s.
    let error = tokio::time::timeout(Duration::from_secs(120), running)
        .await
        .expect("the agent must exit on one shutdown request")
        .unwrap()
        .expect_err("and must report why it could not retire");
    assert!(error.to_string().contains("shutdown budget"), "{error}");
}
```

   `fixture_worker()` is the `WorkerConfig` the file's `ProcessWorkerFixture`
   tests already build; reuse theirs rather than adding one.

2. Run `cargo test -p voom-node-agent a_sigterm_exits_the_agent`. Expect
   `test result: ok. 1 passed`, in roughly 10 s — the default `call` budget.
   Before committing, verify the pre-change failure the same way Task 5 requires:
   `git stash` the production change, confirm the test fails on `Elapsed`,
   restore, confirm it passes. Record both results in the commit message.
3. Run `cargo test -p voom-node-agent` and `just lint`. Expect green.
4. Commit: `test(node-agent): cover the SIGTERM exposure end to end`.

### Acceptance criteria

- One shutdown request against an unanswering control plane exits the agent with
  an error naming the budget, in well under the default tail bound.
- The test uses no name this change introduces, so it compiles pre-change.
- Both R5 witnesses' pre-change failures have been observed, not assumed, and the
  observations are in their commit messages.

## Final verification

1. `just fmt`
2. `just lint` — exit 0, no warnings.
3. `cargo test -p voom-node-agent` — all green.
4. `just ci` — the full suite, including `check-test-layout`,
   `check-paused-time-db`, `check-adr-index`, `doc`, `deny`, `audit`. Expect
   `==> All CI checks passed`. Budget generously: this is minutes, and the
   `prek` pre-push hook re-runs it in an isolated worktree, which can outlast a
   two-minute tool timeout — that is slowness, not a hang.
5. Compare `just test` wall clock against a `main` baseline. The spec predicts
   under a second of added time; a material move means a suite reached a budget
   it was not meant to reach, or a new test took the defaults by mistake.

## Rollback

Every task is a separate commit on `feat/bounded-graceful-shutdown-452`, and none
touches persisted data, a migration, or a public API. Reverting any single commit
leaves the tree building: Task 1's budgets are inert without Tasks 2–6, and Tasks
2–6 each bound one wait independently. Reverting the whole branch restores the
pre-change behaviour exactly, since no configuration field, schema, or wire format
changes.
