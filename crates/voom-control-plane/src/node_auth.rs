use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};

use crate::ControlPlane;

const TOKEN_PREFIX: &str = "voom-node-v1.";
const TOKEN_HASH_PREFIX: &str = "voom-node-token-sha256-v1:";
const TOKEN_HASH_DOMAIN: &str = "voom-node-token-v1:";
const TOKEN_RANDOM_BYTES: usize = 32;
const TOKEN_HINT_LEN: usize = 8;

pub struct GeneratedNodeToken {
    pub plaintext: SecretString,
    pub hash: String,
    pub hint: String,
}

impl fmt::Debug for GeneratedNodeToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeneratedNodeToken")
            .field("plaintext", &"<secret>")
            .field("hash", &"<secret>")
            .field("hint", &self.hint)
            .finish()
    }
}

impl ControlPlane {
    pub(crate) fn generate_node_token(&self) -> GeneratedNodeToken {
        let mut bytes = [0_u8; TOKEN_RANDOM_BYTES];
        {
            let mut guard = self
                .rng
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.fill_bytes(&mut bytes);
        }
        let plaintext = generate_token_from_bytes(bytes);
        let exposed = plaintext.expose_secret();
        GeneratedNodeToken {
            hash: hash_node_token(exposed),
            hint: token_hint(exposed),
            plaintext,
        }
    }
}

pub(crate) fn generate_token_from_bytes(bytes: [u8; TOKEN_RANDOM_BYTES]) -> SecretString {
    let mut token = String::with_capacity(TOKEN_PREFIX.len() + TOKEN_RANDOM_BYTES.div_ceil(3) * 4);
    token.push_str(TOKEN_PREFIX);
    URL_SAFE_NO_PAD.encode_string(bytes, &mut token);
    SecretString::from(token)
}

#[must_use]
pub fn hash_node_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(TOKEN_HASH_DOMAIN.as_bytes());
    hasher.update(token.as_bytes());
    format!("{TOKEN_HASH_PREFIX}{}", hex::encode(hasher.finalize()))
}

#[must_use]
pub fn verify_node_token(token: &str, expected_hash: &str) -> bool {
    constant_time_eq::constant_time_eq(hash_node_token(token).as_bytes(), expected_hash.as_bytes())
}

#[must_use]
pub fn token_hint(token: &str) -> String {
    let hint: String = token.chars().rev().take(TOKEN_HINT_LEN).collect();
    hint.chars().rev().collect()
}

#[cfg(test)]
#[path = "node_auth_test.rs"]
mod tests;
