use super::*;

#[test]
fn valid_policy_fixtures_match_compiled_goldens() {
    for fixture in valid_policy_fixtures() {
        let source = load_policy_fixture(fixture.source_path).unwrap();
        let compiled = crate::compile_policy(&source).unwrap();
        let actual = crate::deterministic_json(&compiled.policy).unwrap();
        let expected = load_json_or_actual_message(fixture.expected_json_path, &actual)
            .unwrap_or_else(|err| {
                unreachable!("{err}");
            });
        assert_eq!(actual, expected, "fixture {}", fixture.source_path);
    }
}

#[test]
fn invalid_policy_fixtures_match_diagnostic_goldens() {
    for fixture in invalid_policy_fixtures() {
        let source = load_policy_fixture(fixture.source_path).unwrap();
        let err = crate::compile_policy(&source).unwrap_err();
        let actual = serde_json::to_value(&err.diagnostics).unwrap();
        let expected = load_json_or_actual_message(fixture.expected_json_path, &actual)
            .unwrap_or_else(|err| {
                unreachable!("{err}");
            });
        assert_eq!(actual, expected, "fixture {}", fixture.source_path);
    }
}

#[test]
fn historical_filtered_exists_compiled_fixture_remains_readable() {
    let value = load_json_fixture("fixtures/compiled/production-normalize-reduced.json").unwrap();
    let policy = serde_json::from_value::<crate::CompiledPolicy>(value).unwrap();

    assert_eq!(policy.slug, "production-normalize-reduced");
}

#[test]
fn canonical_filter_sources_preserve_historical_compiled_behavior() {
    for name in [
        "audio-transcode-eac3",
        "audio-transcode-extract",
        "filter-addressed-tracks",
    ] {
        let historical = format!("fixtures/compiled/historical-track-filter-source/{name}.json");
        let current = format!("fixtures/compiled/{name}.json");
        let historical = without_source_hash(load_json_fixture(&historical).unwrap());
        let current = without_source_hash(load_json_fixture(&current).unwrap());

        assert_eq!(current, historical, "fixture {name}");
    }
}

#[test]
fn historical_escaped_title_compiled_fixture_matches_pre_refactor_compiler() {
    let source = include_str!("../../fixtures/historical/escaped-title-filters.voom");
    let compiled = crate::compile_policy(source).unwrap();
    let actual = crate::deterministic_json(&compiled.policy).unwrap();
    let expected = load_json_or_actual_message(
        "fixtures/compiled/historical-track-filter-source/escaped-title-filters.json",
        &actual,
    )
    .unwrap_or_else(|err| {
        unreachable!("{err}");
    });

    assert_eq!(actual, expected);
    assert_eq!(
        crate::source_hash(source),
        "c4b99a0bae2445fac2f4c2662909d55b91fbb1fe63847dac5fa770541a5e69ba"
    );
    assert_title_values(
        &compiled.policy,
        &[
            r#"Director \"Cut\"#,
            r"Path\\Name",
            r#"\"Quoted\" middle"#,
            r"Other\qPair",
        ],
    );
    let historical = serde_json::from_value::<crate::CompiledPolicy>(expected.clone()).unwrap();
    assert_title_values(
        &historical,
        &[
            r#"Director \"Cut\"#,
            r"Path\\Name",
            r#"\"Quoted\" middle"#,
            r"Other\qPair",
        ],
    );
    assert_eq!(serde_json::to_value(historical).unwrap(), expected);
}

#[test]
fn historical_escaped_title_boundary_fixture_captures_pre_change_compiler() {
    let source = include_str!("../../fixtures/historical/escaped-title-filter-boundaries.voom");
    let compiled = crate::compile_policy(source).unwrap();
    let actual = crate::deterministic_json(&compiled.policy).unwrap();
    let expected = load_json_or_actual_message(
        "fixtures/compiled/historical-track-filter-source/escaped-title-filter-boundaries.json",
        &actual,
    )
    .unwrap_or_else(|err| {
        unreachable!("{err}");
    });

    assert_eq!(actual, expected);
    assert_eq!(
        crate::source_hash(source),
        "03fcaff76ec94729cbb436aebd2711cf52f0329c9aa4be168357c2a59a841e32"
    );
    assert_title_values(&compiled.policy, &[r"Path\\", r"Caf\é"]);
    let historical = serde_json::from_value::<crate::CompiledPolicy>(expected.clone()).unwrap();
    assert_title_values(&historical, &[r"Path\\", r"Caf\é"]);
    assert_eq!(serde_json::to_value(historical).unwrap(), expected);
}

fn without_source_hash(mut value: serde_json::Value) -> serde_json::Value {
    value.as_object_mut().unwrap().remove("source_hash");
    value
}

fn load_json_or_actual_message(
    path: &str,
    actual: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    load_json_fixture(path).map_err(|err| {
        format!(
            "missing or unreadable golden {path}: {err}\n{}",
            serde_json::to_string_pretty(actual).unwrap()
        )
    })
}

fn assert_title_values(policy: &crate::CompiledPolicy, expected: &[&str]) {
    let actual = policy
        .phases
        .iter()
        .map(|phase| {
            let [
                crate::CompiledOperation::KeepTracks {
                    filter: Some(crate::TrackFilter::TitleContains { value }),
                    ..
                },
            ] = phase.operations.as_slice()
            else {
                unreachable!("expected one title-filtered keep operation");
            };
            value.as_str()
        })
        .collect::<Vec<_>>();

    assert_eq!(actual, expected);
}
