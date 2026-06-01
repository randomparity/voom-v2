use super::*;
use secrecy::SecretString;
use voom_core::WorkerId;

fn live(secret: &str) -> WorkerCredentials {
    WorkerCredentials {
        worker_id: WorkerId(42),
        worker_epoch: 7,
        secret: secret.to_string().into(),
    }
}

fn presented(id: u64, epoch: u64, secret: &str) -> PresentedCredentials {
    PresentedCredentials {
        worker_id: WorkerId(id),
        worker_epoch: epoch,
        secret: secret.to_string().into(),
    }
}

#[test]
fn matching_credentials_succeed() {
    let l = live("topsecret");
    let p = presented(42, 7, "topsecret");
    assert!(validate_credentials(&p, &l).is_ok());
}

#[test]
fn wrong_worker_id_rejects_with_unknown_worker_id() {
    let l = live("topsecret");
    let p = presented(43, 7, "topsecret");
    let err = validate_credentials(&p, &l).unwrap_err();
    assert!(matches!(
        err,
        ProtocolError::UnknownWorkerId {
            presented: WorkerId(43)
        }
    ));
}

#[test]
fn wrong_epoch_rejects_with_stale_worker_epoch() {
    let l = live("topsecret");
    let p = presented(42, 6, "topsecret");
    let err = validate_credentials(&p, &l).unwrap_err();
    assert!(matches!(
        err,
        ProtocolError::StaleWorkerEpoch {
            presented: 6,
            current: 7,
        }
    ));
}

#[test]
fn wrong_secret_rejects_with_unauthorized_bearer() {
    let l = live("topsecret");
    let p = presented(42, 7, "wrong");
    let err = validate_credentials(&p, &l).unwrap_err();
    assert!(matches!(err, ProtocolError::UnauthorizedBearer));
}

#[test]
fn debug_redacts_secret() {
    let l = live("supersecret");
    let dbg = format!("{l:?}");
    assert!(
        dbg.contains("<redacted>"),
        "Debug must redact secret; got {dbg}"
    );
    assert!(
        !dbg.contains("supersecret"),
        "Debug must not contain the secret value; got {dbg}"
    );
}

#[test]
fn presented_debug_redacts_secret() {
    let p = presented(42, 7, "anothersecret");
    let dbg = format!("{p:?}");
    assert!(dbg.contains("<redacted>"));
    assert!(!dbg.contains("anothersecret"));
}

#[test]
fn cloned_credentials_match_original() {
    let l = live("clonemepls");
    let c = l.clone();
    let p = PresentedCredentials {
        worker_id: c.worker_id,
        worker_epoch: c.worker_epoch,
        secret: c.secret.expose_secret().to_string().into(),
    };
    assert!(validate_credentials(&p, &l).is_ok());
}

#[test]
fn _assert_secret_string_compiles_in_secrecy() {
    // Confirms our chosen secret wrapper compiles and the trait used at
    // the API boundary (ExposeSecret) is available. Drop-time zeroize
    // is a property of the secrecy crate; we trust the upstream test
    // suite for it.
    let _s: SecretString = "ignored".to_string().into();
}
