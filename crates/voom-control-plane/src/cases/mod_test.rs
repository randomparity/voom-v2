use voom_events::EventKind;
use voom_store::repo::audit::events::{EventFilter, EventRepo, Page};

pub(crate) async fn count(cp: &crate::ControlPlane, kind: EventKind) -> usize {
    cp.events()
        .list(
            EventFilter {
                kind: Some(kind),
                ..EventFilter::default()
            },
            Page {
                limit: 200,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
        .len()
}

pub(crate) async fn cp() -> (crate::ControlPlane, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    let _ = voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
        std::sync::Arc::new(std::sync::Mutex::new(
            voom_core::rng_test_support::FrozenRng::new(u32::MAX),
        )),
    )
    .await
    .unwrap();
    (cp, tmp)
}

/// A `terminal_failure` issue row, projected for the execution-case tests
/// that assert the auto-open path stamped the right severity/priority/status.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TerminalFailureIssueRow {
    pub id: i64,
    pub severity: String,
    pub priority: String,
    pub priority_source: String,
    pub status: String,
    pub dedupe_key: Option<String>,
}

/// All `terminal_failure` issues in the store, oldest first.
pub(crate) async fn terminal_failure_issues(
    cp: &crate::ControlPlane,
) -> Vec<TerminalFailureIssueRow> {
    sqlx::query_as::<_, (i64, String, String, String, String, Option<String>)>(
        "SELECT id, severity, priority, priority_source, status, dedupe_key \
         FROM issues WHERE kind = 'terminal_failure' ORDER BY id",
    )
    .fetch_all(cp.pool_for_test())
    .await
    .unwrap()
    .into_iter()
    .map(
        |(id, severity, priority, priority_source, status, dedupe_key)| TerminalFailureIssueRow {
            id,
            severity,
            priority,
            priority_source,
            status,
            dedupe_key,
        },
    )
    .collect()
}

/// `(link_type, target_id)` for an issue's links, ordered by `link_type`.
pub(crate) async fn issue_link_targets(
    cp: &crate::ControlPlane,
    issue_id: i64,
) -> Vec<(String, i64)> {
    sqlx::query_as::<_, (String, i64)>(
        "SELECT link_type, target_id FROM issue_links \
         WHERE issue_id = ? ORDER BY link_type",
    )
    .bind(issue_id)
    .fetch_all(cp.pool_for_test())
    .await
    .unwrap()
}

/// Builds a single-video mp4/h264 input set whose snapshot is transcodable to
/// hevc, used by both the execute-path and dry-run-path resolution tests.
pub(crate) async fn transcodable_input(
    cp: &crate::ControlPlane,
    slug: &str,
) -> voom_core::PolicyInputSetId {
    let mut draft =
        voom_policy::load_fixture(voom_policy::FixtureName::SyntheticNoncompliantTranscodeNeeded)
            .unwrap();
    draft.slug = slug.to_owned();
    draft.fixture_labels = vec![slug.replace('-', "_")];
    let snapshot = &mut draft.media_snapshots[0];
    snapshot.container = Some("mp4".to_owned());
    snapshot.video_codec = Some("h264".to_owned());
    snapshot.stream_summary = serde_json::json!({ "video_stream_count": 1 });
    cp.create_policy_input_set(draft).await.unwrap().id
}
