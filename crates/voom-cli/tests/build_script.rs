#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result<()> through every assertion"
)]

#[path = "../build.rs"]
mod build_script;

#[test]
fn parse_git_status_extracts_sha_branch_and_dirty_flag() {
    let status = build_script::parse_git_status(
        "# branch.oid abcdef123456789\n# branch.head feature/test\n1 .M N... 100644 100644 100644 a b Cargo.toml\n",
    )
    .unwrap();

    assert_eq!(status.sha.as_deref(), Some("abcdef123456789"));
    assert_eq!(status.branch.as_deref(), Some("feature/test"));
    assert!(status.dirty);
}

#[test]
fn parse_git_status_treats_initial_and_detached_as_absent() {
    let status =
        build_script::parse_git_status("# branch.oid (initial)\n# branch.head (detached)\n")
            .unwrap();

    assert_eq!(status.sha, None);
    assert_eq!(status.branch, None);
    assert!(!status.dirty);
}
