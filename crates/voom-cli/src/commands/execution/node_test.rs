use crate::cli::{NodeKindArg, NodeStatusArg};

#[test]
fn node_kind_arg_maps_to_store_vocab() {
    assert_eq!(NodeKindArg::Local.to_store().as_str(), "local");
    assert_eq!(NodeKindArg::Remote.to_store().as_str(), "remote");
    assert_eq!(NodeKindArg::Synthetic.to_store().as_str(), "synthetic");
}

#[test]
fn node_status_arg_maps_to_store_vocab() {
    assert_eq!(NodeStatusArg::Registered.to_store().as_str(), "registered");
    assert_eq!(NodeStatusArg::Active.to_store().as_str(), "active");
    assert_eq!(NodeStatusArg::Stale.to_store().as_str(), "stale");
    assert_eq!(NodeStatusArg::Retired.to_store().as_str(), "retired");
}
