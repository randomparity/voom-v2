#![expect(
    clippy::print_stderr,
    reason = "benchmark-worker placeholder advertises its scaffold status on stderr (Phase 5 design)"
)]
//! `benchmark-worker` — Sprint 2 Phase 5 placeholder. Real
//! throughput / latency measurement deferred to a follow-up
//! commit per the Phase 5 design.

fn main() {
    eprintln!("benchmark-worker is a Phase 5 follow-up commit");
}
