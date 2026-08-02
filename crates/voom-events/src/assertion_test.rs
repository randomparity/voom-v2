use super::*;

#[test]
fn wire_forms_round_trip_for_every_variant() {
    for kind in [
        AssertionKind::BelongsToWork,
        AssertionKind::BelongsToVariant,
        AssertionKind::SameAsAsset,
        AssertionKind::DuplicateOfAsset,
        AssertionKind::PreferredVariant,
        AssertionKind::UserLabel,
        AssertionKind::ExternalIdMatch,
        AssertionKind::PathRuleMatch,
        AssertionKind::HashMatch,
        AssertionKind::RuntimeSimilarityMatch,
        AssertionKind::FrameFingerprintMatch,
        AssertionKind::AudioFingerprintMatch,
    ] {
        let s = kind.as_str();
        let back = AssertionKind::from_str(s).unwrap();
        assert_eq!(kind, back, "round-trip failed for {s}");
        let json = serde_json::to_value(kind).unwrap();
        assert_eq!(json, s);
        assert_eq!(serde_json::from_value::<AssertionKind>(json).unwrap(), kind);
    }
}

#[test]
fn from_str_rejects_unknown_value() {
    let err = AssertionKind::from_str("not_a_real_assertion").unwrap_err();
    assert!(
        format!("{err}").contains("not_a_real_assertion"),
        "error message must echo the bad token; got: {err}"
    );
}

#[test]
fn try_from_delegates_to_from_str() {
    let k: AssertionKind = "hash_match".try_into().unwrap();
    assert_eq!(k, AssertionKind::HashMatch);
}
