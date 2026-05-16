#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use voom_cli::commands::init::InitData;

#[test]
fn init_first_run_shape() {
    let data = InitData {
        migrations_applied: 1,
        schema_init_at: "2026-05-15T18:23:00.000Z".into(),
        already_initialized: false,
    };
    insta::assert_json_snapshot!("init_first", &data);
}

#[test]
fn init_already_initialized_shape() {
    let data = InitData {
        migrations_applied: 0,
        schema_init_at: "2026-05-15T18:23:00.000Z".into(),
        already_initialized: true,
    };
    insta::assert_json_snapshot!("init_idempotent", &data);
}
