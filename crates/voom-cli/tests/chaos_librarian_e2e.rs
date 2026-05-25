#![expect(
    clippy::unwrap_used,
    reason = "E2E tests fail loudly and preserve paths for diagnosis"
)]

mod support;

use support::chaos_librarian::ChaosLibrarian;

#[test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
fn chaos_librarian_submodule_is_pinned_and_ready() {
    let chaos = ChaosLibrarian::discover().unwrap();
    let readiness = chaos.validate_ready().unwrap();

    assert_eq!(
        readiness.revision,
        "057a4033a3a9ae14fef664ab82f2c31e1a223544"
    );
    assert!(
        readiness.capabilities["ready_for"]["materialize_static"]
            .as_bool()
            .unwrap_or(false)
    );
    assert!(
        readiness.capabilities["ready_for"]["materialize_media_mutations"]
            .as_bool()
            .unwrap_or(false)
    );
}
