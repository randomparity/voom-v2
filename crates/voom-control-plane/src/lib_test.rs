use super::*;

fn fresh_url() -> (tempfile::NamedTempFile, String) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    (tmp, url)
}

#[tokio::test]
async fn open_refuses_missing_database() {
    let tmp = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", tmp.path().join("nope.db").display());
    let err = HealthPlane::open(&url).await.unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::DbUnreachable);
}

#[tokio::test]
async fn health_on_existing_but_uninitialized_db_is_uninitialized() {
    let (_keep, url) = fresh_url();
    voom_store::connect_or_create(&url).await.unwrap();

    let hp = HealthPlane::open(&url).await.unwrap();
    let snap = hp.health().await.unwrap();
    assert_eq!(snap, HealthSnapshot::Uninitialized);
}

#[tokio::test]
async fn init_then_health_reports_current() {
    let (_keep, url) = fresh_url();
    let report = voom_store::init(&url).await.unwrap();
    assert!(!report.already_initialized);

    let cp = ControlPlane::open(&url).await.unwrap();
    let snap = cp.health().await.unwrap();
    match snap {
        HealthSnapshot::Current {
            migration_count,
            schema_init_at: _,
        } => assert_eq!(migration_count, voom_store::expected_migrations()),
        other => panic!("expected Current, got {other:?}"),
    }
}

#[tokio::test]
async fn control_plane_open_rejects_uninitialized_db() {
    let (_keep, url) = fresh_url();
    voom_store::connect_or_create(&url).await.unwrap();
    let err = ControlPlane::open(&url).await.unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::DbPartialSchema);
}

#[tokio::test]
async fn health_plane_open_succeeds_on_uninitialized_db() {
    let (_keep, url) = fresh_url();
    voom_store::connect_or_create(&url).await.unwrap();
    let hp = HealthPlane::open(&url).await.unwrap();
    let snap = hp.health().await.unwrap();
    assert!(
        snap.diagnostic().is_some(),
        "uninitialized DB must produce a diagnostic"
    );
}

#[tokio::test]
async fn second_init_returns_already_initialized() {
    let (_keep, url) = fresh_url();
    voom_store::init(&url).await.unwrap();
    let second = voom_store::init(&url).await.unwrap();
    assert!(second.already_initialized);
    assert_eq!(second.migrations_applied, 0);
}

#[tokio::test]
async fn health_maps_dirty_state() {
    let (_keep, url) = fresh_url();
    voom_store::init(&url).await.unwrap();

    {
        let pool = voom_store::connect(&url).await.unwrap();
        sqlx::query("UPDATE _sqlx_migrations SET success = 0 WHERE version = 1")
            .execute(&pool)
            .await
            .unwrap();
    }

    let hp = HealthPlane::open(&url).await.unwrap();
    let snap = hp.health().await.unwrap();
    match snap {
        HealthSnapshot::Dirty {
            failed_version,
            applied: _,
            expected: _,
        } => assert_eq!(failed_version, 1),
        other => panic!("expected Dirty, got {other:?}"),
    }
}

#[tokio::test]
async fn health_maps_too_new_state() {
    let (_keep, url) = fresh_url();
    voom_store::init(&url).await.unwrap();

    {
        let pool = voom_store::connect(&url).await.unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
             VALUES (99999, 'synthetic-future', strftime('%s','now'), 1, X'00', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    let hp = HealthPlane::open(&url).await.unwrap();
    let snap = hp.health().await.unwrap();
    match snap {
        HealthSnapshot::TooNew { applied, expected } => {
            assert!(applied > expected);
        }
        other => panic!("expected TooNew, got {other:?}"),
    }
}

/// Exhaustive coverage check: every non-Current variant must produce a
/// diagnostic with a non-empty message. Adding a `HealthSnapshot` variant
/// without updating `diagnostic()` fails to compile (the match in
/// `diagnostic()` is exhaustive); this test then catches any new variant
/// that returns an empty or placeholder message.
#[test]
fn diagnostic_covers_every_non_current_variant() {
    let now = OffsetDateTime::UNIX_EPOCH;
    let cases = [
        HealthSnapshot::Uninitialized,
        HealthSnapshot::Partial {
            applied: 0,
            expected: 1,
        },
        HealthSnapshot::TooNew {
            applied: 2,
            expected: 1,
        },
        HealthSnapshot::Dirty {
            failed_version: 1,
            applied: 1,
            expected: 1,
        },
    ];
    for snap in &cases {
        let diag = snap.diagnostic().unwrap_or_else(|| {
            panic!("non-Current variant {snap:?} returned None from diagnostic()")
        });
        assert!(!diag.message.is_empty(), "{snap:?} has empty message");
        assert!(diag.hint.is_some(), "{snap:?} has no hint");
    }

    // Current returns None.
    let current = HealthSnapshot::Current {
        migration_count: 1,
        schema_init_at: now,
    };
    assert!(current.diagnostic().is_none());
}

/// Regression guard for the issue #1 ugliness: `Option<u32>` Debug
/// produced `applied=Some(0)` in operator-facing strings. The ADT
/// fields are plain integers, so the formatted string must not contain
/// `Some(`.
#[test]
fn diagnostic_messages_have_no_debug_options() {
    let snaps = [
        HealthSnapshot::Partial {
            applied: 0,
            expected: 1,
        },
        HealthSnapshot::TooNew {
            applied: 2,
            expected: 1,
        },
        HealthSnapshot::Dirty {
            failed_version: 1,
            applied: 1,
            expected: 1,
        },
    ];
    for snap in &snaps {
        let diag = snap.diagnostic().unwrap();
        assert!(
            !diag.message.contains("Some("),
            "diagnostic message for {snap:?} leaks Option Debug: {msg}",
            msg = diag.message,
        );
    }
}
