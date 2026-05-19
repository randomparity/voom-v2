#![expect(
    clippy::print_stderr,
    reason = "chaos-worker placeholder advertises its scaffold status on stderr (Phase 4 design)"
)]
//! `chaos-worker` — Sprint 2 Phase 4 placeholder. Real failure-mode
//! implementation (crash / stall / malformed / missed-heartbeat /
//! deadline-exceeded) deferred to a follow-up commit per the
//! Phase 4 design.

fn main() {
    eprintln!("chaos-worker is a Phase 4 follow-up commit");
}
