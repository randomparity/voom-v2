use std::sync::{Arc, Mutex};

use secrecy::ExposeSecret;
use voom_core::{SystemClock, rng_test_support::FrozenRng};

use super::*;

#[test]
fn generated_token_uses_v1_prefix_and_256_bits() {
    let token = generate_token_from_bytes([7_u8; 32]).unwrap();
    let exposed = token.expose_secret();
    assert!(exposed.starts_with("voom-node-v1."));
    assert_eq!(exposed.trim_start_matches("voom-node-v1.").len(), 43);
}

#[test]
fn token_hash_uses_versioned_domain_separated_sha256_hex() {
    let hash = hash_node_token("voom-node-v1.test");
    assert_eq!(
        hash,
        "voom-node-token-sha256-v1:08356516626c757dd822687cdc9f324f329761b82869f5bc5a6a297062197c4b"
    );
}

#[test]
fn verification_uses_hash_equality_without_exposing_secret() {
    let hash = hash_node_token("voom-node-v1.valid");
    assert!(verify_node_token("voom-node-v1.valid", &hash));
    assert!(!verify_node_token("voom-node-v1.invalid", &hash));
}

#[test]
fn token_hint_is_short_suffix_only() {
    let hint = token_hint("voom-node-v1.abcdefghijklmnopqrstuvwxyz0123456789");
    assert_eq!(hint, "23456789");
    assert!(!hint.starts_with("voom-node-v1."));
}

#[test]
fn token_hint_handles_non_ascii_without_panicking() {
    assert_eq!(token_hint("ééééa"), "ééééa");
    assert_eq!(token_hint("voom-node-v1.ééééééééa"), "éééééééa");
}

#[tokio::test]
async fn control_plane_generates_token_from_injected_rng() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        Arc::new(SystemClock),
        Arc::new(Mutex::new(FrozenRng::new(0x0707_0707))),
    )
    .await
    .unwrap();

    let generated = cp.generate_node_token().unwrap();
    assert_eq!(
        generated.plaintext.expose_secret(),
        generate_token_from_bytes([7_u8; 32])
            .unwrap()
            .expose_secret()
    );
    assert_eq!(
        generated.hash,
        hash_node_token(generated.plaintext.expose_secret())
    );
    assert_eq!(
        generated.hint,
        token_hint(generated.plaintext.expose_secret())
    );
}
