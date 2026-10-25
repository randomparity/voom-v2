use serde_json::Value;
use std::path::Path;

const EXPECTED: [(&str, bool); 7] = [
    ("00-mp4-start.mp4", false),
    ("01-mp4-end.mp4", false),
    ("02-mkv-cues.mkv", true),
    ("03-mp3-16.mp3", true),
    ("10-healthy.mkv", true),
    ("10-healthy.mp3", true),
    ("10-healthy.mp4", true),
];

pub async fn assert_outcomes(url: &str, evidence: &Value, observed: &Value) {
    let pool = voom_store::connect(url).await.unwrap();
    let observations: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT provider_relative_locator, evidence_json FROM scan_observations ORDER BY provider_relative_locator"
    ).fetch_all(&pool).await.unwrap();
    assert_eq!(observations.len(), EXPECTED.len());
    for ((locator, persisted), (name, success)) in observations.iter().zip(EXPECTED) {
        assert_eq!(locator, name);
        let calls: Vec<_> = evidence["probe_invocations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|call| {
                call["args"].as_array().unwrap().iter().any(|arg| {
                    arg.as_str().is_some_and(|arg| {
                        Path::new(arg).file_name().is_some_and(|file| file == name)
                    })
                })
            })
            .collect();
        assert_eq!(calls.len(), 1, "one real worker probe for {name}");
        assert_eq!(
            calls[0]["exit_code"].as_i64().unwrap() == 0,
            success,
            "{name}"
        );
        assert_eq!(
            persisted.is_some(),
            success,
            "failed probes cannot invent evidence: {name}"
        );
        if let Some(persisted) = persisted {
            let facts: serde_json::Value = serde_json::from_str(persisted).unwrap();
            assert!(
                facts["content_hash"]
                    .as_str()
                    .unwrap()
                    .starts_with("blake3:")
            );
            assert!(
                facts["probe_snapshot"]["raw"]["ffprobe_json"]["streams"]
                    .as_array()
                    .is_some_and(|streams| !streams.is_empty())
            );
        }
    }
    let snapshots: i64 = sqlx::query_scalar("SELECT count(*) FROM media_snapshots")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        snapshots, 5,
        "both failed MP4 probes settle without publishing snapshots"
    );
    pool.close().await;
    assert_published(observed);
}

fn assert_published(observed: &Value) {
    let mut actual: Vec<_> = observed["assets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|asset| {
            assert!(!asset["probed"].is_null());
            asset["current_path"].as_str().unwrap()
        })
        .collect();
    actual.sort_unstable();
    assert_eq!(
        actual,
        EXPECTED
            .iter()
            .filter_map(|(name, success)| success.then_some(*name))
            .collect::<Vec<_>>()
    );
}
