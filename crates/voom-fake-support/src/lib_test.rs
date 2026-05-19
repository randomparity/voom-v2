use super::*;

#[test]
fn scenario_round_trips() {
    let s = Scenario {
        scenario: "test-basic".into(),
        events: vec![
            ScenarioEvent::DiscoverFile {
                path: "/a/b".into(),
                size: 42,
            },
            ScenarioEvent::ScanComplete { duration_ms: 100 },
        ],
    };
    let json = serde_json::to_string(&s).unwrap();
    let back: Scenario = serde_json::from_str(&json).unwrap();
    assert_eq!(s, back);
}

#[test]
fn scenario_player_yields_events_in_order() {
    let s = Scenario {
        scenario: "t".into(),
        events: vec![
            ScenarioEvent::DiscoverFile {
                path: "/x".into(),
                size: 1,
            },
            ScenarioEvent::ScanComplete { duration_ms: 2 },
        ],
    };
    let mut p = ScenarioPlayer::new(s);
    assert!(matches!(
        p.next_event(),
        Some(ScenarioEvent::DiscoverFile { .. })
    ));
    assert!(matches!(
        p.next_event(),
        Some(ScenarioEvent::ScanComplete { .. })
    ));
    assert!(p.next_event().is_none());
}

#[test]
fn scenario_rejects_unknown_top_level_fields() {
    let raw = r#"{"scenario":"t","events":[],"unknown_extra":true}"#;
    let res: Result<Scenario, _> = serde_json::from_str(raw);
    assert!(res.is_err());
}
