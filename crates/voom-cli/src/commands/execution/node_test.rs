use clap::Parser;

use crate::cli::{Cli, Command, NodeCommand, NodeIncarnationCommand, NodeKindArg, NodeStatusArg};

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

#[test]
fn node_incarnation_list_defaults_and_bounds_limit() {
    let cli =
        Cli::try_parse_from(["voom", "node", "incarnation", "list", "--node-id", "7"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Node(NodeCommand::Incarnation(NodeIncarnationCommand::List {
            node_id: 7,
            limit: 100,
        }))
    ));

    for limit in ["0", "1001"] {
        assert!(
            Cli::try_parse_from([
                "voom",
                "node",
                "incarnation",
                "list",
                "--node-id",
                "7",
                "--limit",
                limit,
            ])
            .is_err()
        );
    }
}
