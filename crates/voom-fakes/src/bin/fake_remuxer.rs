#![expect(
    clippy::print_stderr,
    reason = "fake-* placeholder advertises its scaffold status on stderr (Phase 3 design §2)"
)]
//! `fake_remuxer` — Sprint 2 Phase 3 placeholder. Real implementation deferred
//! to a follow-up commit per the Phase 3 design.

fn main() {
    eprintln!("fake_remuxer is a Phase 3 follow-up commit");
}
