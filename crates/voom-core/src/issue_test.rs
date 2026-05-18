use super::*;
use std::str::FromStr;

#[test]
fn severity_round_trips() {
    for s in [
        IssueSeverity::Critical,
        IssueSeverity::High,
        IssueSeverity::Medium,
        IssueSeverity::Low,
        IssueSeverity::Info,
    ] {
        assert_eq!(IssueSeverity::parse(s.as_str()).unwrap(), s);
        assert_eq!(IssueSeverity::from_str(s.as_str()).unwrap(), s);
    }
}

#[test]
fn priority_round_trips() {
    for p in [
        IssuePriority::Urgent,
        IssuePriority::High,
        IssuePriority::Normal,
        IssuePriority::Low,
        IssuePriority::Someday,
    ] {
        assert_eq!(IssuePriority::parse(p.as_str()).unwrap(), p);
        assert_eq!(IssuePriority::from_str(p.as_str()).unwrap(), p);
    }
}

#[test]
fn parse_rejects_unknown_string() {
    assert!(IssueSeverity::parse("nope").is_err());
    assert!(IssuePriority::parse("now").is_err());
}

#[test]
fn serde_uses_snake_case_wire_format() {
    let s = serde_json::to_string(&IssueSeverity::Critical).unwrap();
    assert_eq!(s, "\"critical\"");
    let p = serde_json::to_string(&IssuePriority::Someday).unwrap();
    assert_eq!(p, "\"someday\"");
}
