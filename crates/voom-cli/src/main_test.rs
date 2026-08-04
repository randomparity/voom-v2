use super::*;

#[test]
fn run_codes_map_to_public_exit_contract() {
    let cases = [
        (0, Exit::Ok),
        (1, Exit::BadArgs),
        (-1, Exit::Failure),
        (2, Exit::Failure),
        (3, Exit::Failure),
        (i32::MAX, Exit::Failure),
    ];

    for (code, expected) in cases {
        assert_eq!(
            Exit::from_run_code(code),
            expected,
            "unexpected mapping for run code {code}"
        );
    }
}
