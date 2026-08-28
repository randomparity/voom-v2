//! The control-path budget ladder, asserted rung by rung.
//!
//! A node agent's call into the control plane passes through nested waits. Each
//! one is individually defensible; what broke in #592 is the *relationship*
//! between them. Under write contention the innermost wait engaged, and every
//! layer above it gave up before the layer beneath could report — so the failure
//! surfaced as an unexplained hang with no error from any layer.
//!
//! The rule this file enforces:
//!
//! > **An observer's budget must exceed the budget of what it observes.**
//!
//! The values stay defined next to the code that uses them, with the reasoning
//! for each magnitude. This file owns only the ordering, which belongs to no
//! single layer and is what no single layer can check.
//!
//! ```text
//!   LOCK_WAIT_BUDGET               30s        one statement waiting on SQLite's write lock
//!     < POOL_ACQUIRE_BUDGET        45s        waiting for a connection that may hold a lock wait
//!       < production_request_budget()  153.75s  every attempt, plus the backoff between them
//!         < any external observer            shutdown grace, operator runbook, test hang guard
//! ```
//!
//! [`REQUEST_TIMEOUT`] is deliberately absent as a rung. A single attempt is
//! *not* required to outlast the server's internal budget: retrying is the
//! mechanism by which the logical call outlasts it, and raising the per-attempt
//! timeout to cover the server would multiply the total — leaving an agent
//! unresponsive to SIGTERM for even longer, which is the exposure in #452. The
//! residual cost is real and accepted: a slow-but-working server is abandoned
//! at [`REQUEST_TIMEOUT`] and retried into the same contention, adding load.
//! Shrinking the server-side budgets so a whole call fits inside one attempt is
//! the better long-term answer; it collides with the `busy_timeout >= 30s`
//! floor in `voom-store/src/pool_test.rs` and belongs with the #592 fix.
//!
//! The shutdown budgets are the one deliberate **inversion** of that rule, and
//! they are recorded here for the same reason every other rung is: so the
//! relationship is checked rather than rediscovered. See ADR 0088.
//!
//! ```text
//!   ShutdownBudgets::DEFAULT.call   10s   INVERTED: below what it observes
//!     < production_request_budget()       during shutdown the agent must exit,
//!                                         and the failure underneath is one it
//!                                         can no longer act on
//! ```
//!
//! Adding a rung means adding it here too. A layer absent from this file is a
//! layer nobody is checking.

use std::time::Duration;

use voom_node_agent::client::{REQUEST_TIMEOUT, production_request_budget};
use voom_node_agent::runtime::ShutdownBudgets;
use voom_store::pool::{LOCK_WAIT_BUDGET, POOL_ACQUIRE_BUDGET};

/// Assert one rung, naming both sides so a failure says which relationship
/// broke rather than printing two bare durations.
#[track_caller]
fn assert_outlasts(outer_name: &str, outer: Duration, inner_name: &str, inner: Duration) {
    assert!(
        outer > inner,
        "{outer_name} ({outer:?}) must outlast {inner_name} ({inner:?}); an observer that \
         expires no later than what it observes reports a timeout of its own instead of the \
         failure underneath it (see #592)"
    );
}

#[test]
fn pool_acquire_outlasts_a_lock_wait() {
    // A BEGIN IMMEDIATE path holds its pooled connection across the whole lock
    // wait, so a caller queued behind it waits at least that long. A pool that
    // gave up first would report "pool exhausted" for what is really contention.
    assert_outlasts(
        "POOL_ACQUIRE_BUDGET",
        POOL_ACQUIRE_BUDGET,
        "LOCK_WAIT_BUDGET",
        LOCK_WAIT_BUDGET,
    );
}

#[test]
fn the_retry_budget_outlasts_the_server_side_work_it_observes() {
    // The logical call is the observer, not one attempt. A control-plane handler
    // needs a pooled connection before it can do anything, so a call that gave
    // up sooner would never see the server's error at all.
    assert_outlasts(
        "production_request_budget()",
        production_request_budget(),
        "POOL_ACQUIRE_BUDGET",
        POOL_ACQUIRE_BUDGET,
    );
}

#[test]
fn the_retry_budget_outlasts_a_single_attempt() {
    assert_outlasts(
        "production_request_budget()",
        production_request_budget(),
        "REQUEST_TIMEOUT",
        REQUEST_TIMEOUT,
    );
}

/// The retry budget is the number callers get wrong, so pin its magnitude
/// rather than only its ordering: five attempts at 30s plus capped backoff.
/// A change that quietly multiplies how long an agent can be stuck should fail
/// here and be argued for, not land as a one-line constant edit.
#[test]
fn the_retry_budget_is_the_sum_of_its_attempts_and_backoff() {
    assert_eq!(
        production_request_budget(),
        Duration::from_secs(150) + Duration::from_millis(3750),
    );
}

#[test]
fn the_shutdown_budgets_deliberately_invert_the_ladder() {
    // Every other rung asserts an observer outlasts what it observes. This one asserts
    // the opposite, and that is the decision: during shutdown the agent's obligation is
    // to exit, not to diagnose. See docs/adr/0088-bounded-node-agent-shutdown.md.
    //
    // The magnitudes — the 90s ceiling and the published `grace + 26` arithmetic — are
    // asserted in runtime_test.rs. This file owns only the ordering.
    assert!(
        ShutdownBudgets::DEFAULT.call < production_request_budget(),
        "the shutdown budget ({:?}) is meant to cut a retrying call ({:?}) short",
        ShutdownBudgets::DEFAULT.call,
        production_request_budget(),
    );
}
