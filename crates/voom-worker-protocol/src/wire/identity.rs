use constant_time_eq::constant_time_eq;
use rand::TryRngCore;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use voom_core::WorkerId;

use crate::{ProtocolError, WorkerCredentials, negotiate};

const IDENTITY_CONTEXT: &str = "voom-worker-identity-v1";
const CHALLENGE_BYTES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerIdentityRequest {
    pub offered: u32,
    pub challenge: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerIdentityResponse {
    pub worker_id: WorkerId,
    pub worker_epoch: u64,
    pub protocol_version: u32,
    pub proof: String,
}

pub fn generate_identity_challenge<R>(rng: &mut R) -> Result<String, ProtocolError>
where
    R: TryRngCore + ?Sized,
{
    let mut challenge = [0_u8; CHALLENGE_BYTES];
    rng.try_fill_bytes(&mut challenge)
        .map_err(|error| ProtocolError::InvalidPayload {
            detail: format!("identity challenge RNG: {error}"),
        })?;
    Ok(hex::encode(challenge))
}

pub fn identity_response(
    request: &WorkerIdentityRequest,
    credentials: &WorkerCredentials,
) -> Result<WorkerIdentityResponse, ProtocolError> {
    negotiate(request.offered)?;
    let challenge = decode_challenge(&request.challenge)?;
    let mut response = WorkerIdentityResponse {
        worker_id: credentials.worker_id,
        worker_epoch: credentials.worker_epoch,
        protocol_version: voom_core::PROTOCOL_VERSION,
        proof: String::new(),
    };
    response.proof = hex::encode(identity_proof(
        request.offered,
        &challenge,
        &response,
        credentials.secret.expose_secret(),
    ));
    Ok(response)
}

pub fn verify_identity_response(
    request: &WorkerIdentityRequest,
    response: &WorkerIdentityResponse,
    expected: &WorkerCredentials,
) -> Result<(), ProtocolError> {
    let challenge = decode_challenge(&request.challenge)?;
    let presented =
        hex::decode(&response.proof).map_err(|error| ProtocolError::InvalidPayload {
            detail: format!("identity proof must be hex: {error}"),
        })?;
    let expected_proof = identity_proof(
        request.offered,
        &challenge,
        response,
        expected.secret.expose_secret(),
    );
    if !constant_time_eq(&presented, &expected_proof) {
        return Err(ProtocolError::IdentityProofMismatch);
    }
    if response.protocol_version != request.offered {
        return Err(ProtocolError::UnsupportedProtocolVersion {
            offered: response.protocol_version,
            expected: request.offered,
        });
    }
    if response.worker_id != expected.worker_id {
        return Err(ProtocolError::UnknownWorkerId {
            presented: response.worker_id,
        });
    }
    if response.worker_epoch != expected.worker_epoch {
        return Err(ProtocolError::StaleWorkerEpoch {
            presented: response.worker_epoch,
            current: expected.worker_epoch,
        });
    }
    Ok(())
}

fn decode_challenge(challenge: &str) -> Result<[u8; CHALLENGE_BYTES], ProtocolError> {
    let decoded = hex::decode(challenge).map_err(|error| ProtocolError::InvalidPayload {
        detail: format!("identity challenge must be hex: {error}"),
    })?;
    decoded
        .try_into()
        .map_err(|_| ProtocolError::InvalidPayload {
            detail: format!("identity challenge must contain exactly {CHALLENGE_BYTES} bytes"),
        })
}

fn identity_proof(
    offered: u32,
    challenge: &[u8; CHALLENGE_BYTES],
    response: &WorkerIdentityResponse,
    secret: &str,
) -> [u8; 32] {
    let key = blake3::derive_key(IDENTITY_CONTEXT, secret.as_bytes());
    let mut proof = blake3::Hasher::new_keyed(&key);
    proof.update(IDENTITY_CONTEXT.as_bytes());
    proof.update(&offered.to_be_bytes());
    proof.update(challenge);
    proof.update(&response.worker_id.0.to_be_bytes());
    proof.update(&response.worker_epoch.to_be_bytes());
    proof.update(&response.protocol_version.to_be_bytes());
    *proof.finalize().as_bytes()
}

#[cfg(test)]
#[path = "identity_test.rs"]
mod tests;
