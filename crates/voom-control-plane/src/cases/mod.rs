//! `ControlPlane` use cases. Each method composes a repo `_in_tx` write
//! with `EventRepo::append_in_tx` inside one transaction so every M1
//! state transition produces exactly one event row.

pub mod artifacts;
pub mod jobs;
pub mod leases;
pub mod tickets;
pub mod workers;

#[cfg(test)]
pub(crate) async fn cp() -> (crate::ControlPlane, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    let _ = voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = crate::ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();
    (cp, tmp)
}
