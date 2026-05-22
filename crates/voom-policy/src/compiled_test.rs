use super::*;

#[test]
fn source_hash_uses_exact_bytes() {
    let a = source_hash("policy \"p\" { phase a {} }");
    let b = source_hash("policy \"p\" {\n phase a {}\n}");
    assert_ne!(a, b);
}

#[test]
fn compiled_json_is_deterministic() {
    let policy = CompiledPolicy::minimal_for_test("p", "hash");
    let first = deterministic_json(&policy).unwrap();
    let second = deterministic_json(&policy).unwrap();
    assert_eq!(first, second);
}
