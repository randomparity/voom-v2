use std::sync::Arc;

use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use tokio::sync::Notify;
use voom_test_support::TempDatabase;

use super::*;

async fn checked_pool() -> (SqlitePool, TempDatabase) {
    let database = TempDatabase::new().unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&sqlite_url_for(database.path()))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE checked_values (value INTEGER CHECK (value > 0))")
        .execute(&pool)
        .await
        .unwrap();
    (pool, database)
}

async fn assert_invalid_value_is_rejected(pool: &SqlitePool) {
    let result = sqlx::query("INSERT INTO checked_values (value) VALUES (-1)")
        .execute(pool)
        .await;
    assert!(
        result.is_err(),
        "reacquired connection must enforce CHECK constraints"
    );
}

#[tokio::test]
async fn operation_error_restores_check_constraints_before_pool_reuse() {
    let (pool, _database) = checked_pool().await;

    let result: Result<(), sqlx::Error> = with_check_constraints_disabled(&pool, |_| {
        Box::pin(async { Err(sqlx::Error::Protocol("operation failed".to_owned())) })
    })
    .await;

    assert!(matches!(
        result,
        Err(sqlx::Error::Protocol(message)) if message == "operation failed"
    ));
    assert_invalid_value_is_rejected(&pool).await;
}

#[tokio::test]
async fn cancelled_operation_never_returns_a_tainted_connection_to_the_pool() {
    let (pool, _database) = checked_pool().await;
    let entered = Arc::new(Notify::new());
    let pending = Arc::new(Notify::new());
    let operation_entered = Arc::clone(&entered);
    let operation_pending = Arc::clone(&pending);
    let task_pool = pool.clone();

    let task = tokio::spawn(async move {
        with_check_constraints_disabled(&task_pool, move |_| {
            Box::pin(async move {
                operation_entered.notify_one();
                operation_pending.notified().await;
                Ok(())
            })
        })
        .await
    });

    entered.notified().await;
    task.abort();
    let _ = task.await;

    assert_invalid_value_is_rejected(&pool).await;
}

#[tokio::test]
async fn reset_failure_discards_the_tainted_connection() {
    let (pool, _database) = checked_pool().await;
    let result: Result<(), sqlx::Error> = with_check_constraints_disabled(&pool, |connection| {
        Box::pin(async move {
            let mut handle = connection.lock_handle().await?;
            handle.set_progress_handler(1, || false);
            Ok(())
        })
    })
    .await;

    assert!(matches!(
        result,
        Err(sqlx::Error::Database(error)) if error.message() == "interrupted"
    ));
    assert_invalid_value_is_rejected(&pool).await;
}
