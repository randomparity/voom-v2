use super::*;
use crate::repo::execution::workers::{NewWorker, SqliteWorkerRepo, WorkerKind};
use crate::test_support::fresh_initialized_pool_at;

const NOW: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

async fn repos() -> (
    voom_test_support::TempDatabase,
    SqliteWorkerRepo,
    SqliteAcceleratorClaimRepo,
) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (
        tmp,
        SqliteWorkerRepo::new(pool.clone()),
        SqliteAcceleratorClaimRepo::new(pool),
    )
}

async fn worker(repo: &SqliteWorkerRepo, name: &str) -> WorkerId {
    repo.register(NewWorker {
        name: name.to_owned(),
        kind: WorkerKind::Local,
        registered_at: NOW,
        node_id: None,
    })
    .await
    .unwrap()
    .id
}

fn claim(worker_id: WorkerId) -> NewAcceleratorClaim {
    NewAcceleratorClaim {
        hardware_token: "nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        worker_id,
        boot_id: "boot-id".to_owned(),
        supervisor_pid: 123,
        supervisor_start_ticks: 456,
        process_group_id: 123,
        capacity: 4,
        claimed_at: NOW,
    }
}

#[tokio::test]
async fn claim_is_unique_and_round_trips_process_identity() {
    let (_tmp, workers, claims) = repos().await;
    let first = worker(&workers, "first").await;
    let second = worker(&workers, "second").await;
    let mut tx = claims.pool.begin().await.unwrap();
    let created = claims.claim_in_tx(&mut tx, claim(first)).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        claims.get(&created.hardware_token).await.unwrap(),
        Some(created)
    );

    let mut tx = claims.pool.begin().await.unwrap();
    let error = claims
        .claim_in_tx(&mut tx, claim(second))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "CONFLICT");
}

#[tokio::test]
async fn retiring_owner_releases_claim_in_same_transaction() {
    let (_tmp, workers, claims) = repos().await;
    let owner = worker(&workers, "owner").await;
    let mut tx = claims.pool.begin().await.unwrap();
    claims.claim_in_tx(&mut tx, claim(owner)).await.unwrap();
    tx.commit().await.unwrap();

    workers.retire(owner, 0, NOW).await.unwrap();

    assert!(
        claims
            .get(&claim(owner).hardware_token)
            .await
            .unwrap()
            .is_none()
    );
}
