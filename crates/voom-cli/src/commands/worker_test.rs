use crate::cli::{WorkerKindArg, WorkerStatusArg};

#[test]
fn worker_kind_arg_maps_to_store_vocab() {
    assert_eq!(WorkerKindArg::Local.to_store().as_str(), "local");
    assert_eq!(WorkerKindArg::Remote.to_store().as_str(), "remote");
    assert_eq!(WorkerKindArg::Synthetic.to_store().as_str(), "synthetic");
}

#[test]
fn worker_status_arg_maps_to_store_vocab() {
    assert_eq!(
        WorkerStatusArg::Registered.to_store().as_str(),
        "registered"
    );
    assert_eq!(WorkerStatusArg::Active.to_store().as_str(), "active");
    assert_eq!(WorkerStatusArg::Stale.to_store().as_str(), "stale");
    assert_eq!(WorkerStatusArg::Retired.to_store().as_str(), "retired");
}
