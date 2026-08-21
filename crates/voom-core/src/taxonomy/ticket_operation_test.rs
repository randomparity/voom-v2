use super::*;

#[test]
fn ticket_operation_accepts_known_operation_tokens() {
    assert_eq!(
        TicketOperation::new("synthetic.workflow.operation.hash_file")
            .unwrap()
            .as_str(),
        "synthetic.workflow.operation.hash_file"
    );
    assert_eq!(
        TicketOperation::from(OperationKind::ProbeFile).as_str(),
        "probe_file"
    );
}

#[test]
fn ticket_operation_rejects_empty_and_path_like_tokens() {
    assert!(TicketOperation::new("").is_err());
    assert!(TicketOperation::new("probe/file").is_err());
}

fn normalize(token: &str) -> NormalizedTicketOperation {
    TicketOperation::new(token).unwrap().normalize()
}

#[test]
fn a_bare_known_token_normalizes_to_known_and_not_namespaced() {
    assert_eq!(
        normalize("probe_file"),
        NormalizedTicketOperation::Known {
            kind: OperationKind::ProbeFile,
            namespaced: false,
        }
    );
}

#[test]
fn a_namespaced_known_token_normalizes_to_known_and_namespaced() {
    assert_eq!(
        normalize("synthetic.workflow.operation.probe_file"),
        NormalizedTicketOperation::Known {
            kind: OperationKind::ProbeFile,
            namespaced: true,
        }
    );
}

#[test]
fn tokens_outside_every_reserved_namespace_stay_custom_local() {
    for token in ["disk.test", "noop"] {
        assert_eq!(
            normalize(token),
            NormalizedTicketOperation::CustomLocal(TicketOperation::new(token).unwrap()),
            "classification of {token}"
        );
    }
}

#[test]
fn namespaced_tokens_no_operation_claims_are_unknown_namespaced() {
    // The empty suffix is the case that matters: it is a well-formed
    // TicketOperation, and nothing outside the namespace can produce it.
    for token in [
        "synthetic.workflow.operation.bogus",
        "synthetic.workflow.operation.",
    ] {
        assert_eq!(
            normalize(token),
            NormalizedTicketOperation::UnknownNamespaced(TicketOperation::new(token).unwrap()),
            "classification of {token}"
        );
    }
}

#[test]
fn both_encodings_of_every_operation_agree_on_kind_and_matching_token() {
    for kind in OperationKind::ALL {
        let bare = normalize(kind.as_str());
        let namespaced = normalize(&format!("{WORKFLOW_OPERATION_NAMESPACE}{}", kind.as_str()));

        assert_eq!(bare.operation_kind(), Some(*kind));
        assert_eq!(namespaced.operation_kind(), Some(*kind));
        assert_eq!(bare.matching_token().as_str(), kind.as_str());
        assert_eq!(namespaced.matching_token().as_str(), kind.as_str());
    }
}

#[test]
fn matching_token_never_fabricates_a_bare_token_for_an_unrecognized_operation() {
    // The deleted store helper stripped the namespace off an unknown token and
    // matched capability rows against the bare suffix. Nothing may do that.
    for token in [
        "synthetic.workflow.operation.bogus",
        "synthetic.workflow.operation.",
        "disk.test",
        "noop",
    ] {
        let normalized = normalize(token);
        assert_eq!(normalized.operation_kind(), None, "kind of {token}");
        assert_eq!(
            normalized.matching_token().as_str(),
            token,
            "matching token of {token}"
        );
    }
}
