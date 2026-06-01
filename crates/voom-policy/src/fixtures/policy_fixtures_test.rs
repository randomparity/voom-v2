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
