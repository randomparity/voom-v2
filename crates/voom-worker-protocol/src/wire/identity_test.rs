use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use voom_core::WorkerId;

use super::*;
use crate::{ProtocolError, WorkerCredentials};

struct SequenceRng(u8);

impl RngCore for SequenceRng {
    fn next_u32(&mut self) -> u32 {
        let mut bytes = [0_u8; 4];
        self.fill_bytes(&mut bytes);
        u32::from_le_bytes(bytes)
    }

    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0_u8; 8];
        self.fill_bytes(&mut bytes);
        u64::from_le_bytes(bytes)
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        for byte in destination {
            *byte = self.0;
            self.0 = self.0.wrapping_add(1);
        }
    }
}

fn credentials(secret: &str) -> WorkerCredentials {
    WorkerCredentials {
        worker_id: WorkerId(7),
        worker_epoch: 3,
        secret: SecretString::from(secret),
    }
}

fn request(challenge: &str) -> WorkerIdentityRequest {
    WorkerIdentityRequest {
        offered: voom_core::PROTOCOL_VERSION,
        challenge: challenge.to_owned(),
    }
}

#[test]
fn identity_request_and_response_reject_unknown_fields() {
    let request = r#"{"offered":2,"challenge":"00000000000000000000000000000000","extra":true}"#;
    assert!(serde_json::from_str::<WorkerIdentityRequest>(request).is_err());

    let response =
        r#"{"worker_id":7,"worker_epoch":3,"protocol_version":2,"proof":"00","extra":true}"#;
    assert!(serde_json::from_str::<WorkerIdentityResponse>(response).is_err());
}

#[test]
fn identity_proof_authenticates_expected_worker() {
    let credentials = credentials("secret");
    let request = request("000102030405060708090a0b0c0d0e0f");
    let response = identity_response(&request, &credentials).unwrap();

    verify_identity_response(&request, &response, &credentials).unwrap();
}

#[test]
fn identity_proof_rejects_wrong_secret() {
    let server = credentials("server-secret");
    let expected = credentials("other-secret");
    let request = request("000102030405060708090a0b0c0d0e0f");
    let response = identity_response(&request, &server).unwrap();

    assert_eq!(
        verify_identity_response(&request, &response, &expected),
        Err(ProtocolError::IdentityProofMismatch)
    );
}

#[test]
fn identity_proof_requires_the_expected_worker_id() {
    let server = credentials("secret");
    let mut expected = credentials("secret");
    expected.worker_id = WorkerId(8);
    let request = request("000102030405060708090a0b0c0d0e0f");
    let response = identity_response(&request, &server).unwrap();

    assert_eq!(
        verify_identity_response(&request, &response, &expected),
        Err(ProtocolError::UnknownWorkerId {
            presented: server.worker_id
        })
    );
}

#[test]
fn identity_proof_requires_the_expected_worker_epoch() {
    let server = credentials("secret");
    let mut expected = credentials("secret");
    expected.worker_epoch = 4;
    let request = request("000102030405060708090a0b0c0d0e0f");
    let response = identity_response(&request, &server).unwrap();

    assert_eq!(
        verify_identity_response(&request, &response, &expected),
        Err(ProtocolError::StaleWorkerEpoch {
            presented: server.worker_epoch,
            current: expected.worker_epoch,
        })
    );
}

#[test]
fn identity_proof_requires_the_offered_protocol_version() {
    let credentials = credentials("secret");
    let request = request("000102030405060708090a0b0c0d0e0f");
    let challenge = decode_challenge(&request.challenge).unwrap();
    let mut response = identity_response(&request, &credentials).unwrap();
    response.protocol_version += 1;
    response.proof = hex::encode(identity_proof(
        request.offered,
        &challenge,
        &response,
        credentials.secret.expose_secret(),
    ));

    assert_eq!(
        verify_identity_response(&request, &response, &credentials),
        Err(ProtocolError::UnsupportedProtocolVersion {
            offered: response.protocol_version,
            expected: request.offered,
        })
    );
}

#[test]
fn identity_proof_rejects_malformed_challenges_and_proofs() {
    let credentials = credentials("secret");
    let malformed_request = request("not-hex");
    assert!(identity_response(&malformed_request, &credentials).is_err());

    let request = request("000102030405060708090a0b0c0d0e0f");
    let mut response = identity_response(&request, &credentials).unwrap();
    response.proof = "not-hex".to_owned();
    assert!(verify_identity_response(&request, &response, &credentials).is_err());
}

#[test]
fn identity_proof_cannot_be_replayed_for_a_new_challenge() {
    let credentials = credentials("secret");
    let first = request("000102030405060708090a0b0c0d0e0f");
    let second = request("101112131415161718191a1b1c1d1e1f");
    let captured = identity_response(&first, &credentials).unwrap();

    assert_eq!(
        verify_identity_response(&second, &captured, &credentials),
        Err(ProtocolError::IdentityProofMismatch)
    );
}

#[test]
fn challenge_generation_consumes_fresh_rng_bytes() {
    let mut rng = SequenceRng(1);

    let first = generate_identity_challenge(&mut rng).unwrap();
    let second = generate_identity_challenge(&mut rng).unwrap();

    assert_eq!(first.len(), 32);
    assert_eq!(second.len(), 32);
    assert_ne!(first, second);
}
