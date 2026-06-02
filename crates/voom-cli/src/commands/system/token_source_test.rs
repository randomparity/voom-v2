use std::path::PathBuf;

use voom_core::ErrorCode;

use super::*;

#[tokio::test]
async fn token_file_reads_trimmed_token() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("node.token");
    tokio::fs::write(&path, "voom-node-v1.file\n")
        .await
        .unwrap();
    let token = read_token(&TokenSourceArgs {
        token_file: Some(path),
        token_env: None,
        token_stdin: false,
    })
    .await
    .unwrap();
    assert_eq!(token, "voom-node-v1.file");
}

#[test]
fn token_source_rejects_zero_or_multiple_sources_as_bad_args() {
    assert_eq!(
        validate_token_source(&TokenSourceArgs {
            token_file: None,
            token_env: None,
            token_stdin: false,
        })
        .unwrap_err()
        .code(),
        ErrorCode::BadArgs
    );
    assert_eq!(
        validate_token_source(&TokenSourceArgs {
            token_file: Some(PathBuf::from("token")),
            token_env: Some("VOOM_TOKEN".to_owned()),
            token_stdin: false,
        })
        .unwrap_err()
        .code(),
        ErrorCode::BadArgs
    );
}
