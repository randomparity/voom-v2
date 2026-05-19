#![expect(
    clippy::print_stderr,
    reason = "fake-* placeholder advertises its scaffold status on stderr (Phase 3 design §2)"
)]
//! `fake_use_lease_provider` — Sprint 2 Phase 3 placeholder. Real implementation deferred
//! to a follow-up commit per the Phase 3 design.

fn main() {
    eprintln!("fake_use_lease_provider is a Phase 3 follow-up commit");
}
