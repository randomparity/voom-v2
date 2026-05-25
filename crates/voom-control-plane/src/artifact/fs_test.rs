use super::*;

use std::path::Path;

use voom_core::{ErrorCode, VoomError};

#[tokio::test]
async fn canonical_new_leaf_resolves_existing_parent_and_rejects_existing_leaf() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).unwrap();

    let canonical = canonical_new_leaf_no_symlink(nested.join("..").join("staged.bin"))
        .await
        .unwrap();
    assert_eq!(
        canonical,
        dir.path().canonicalize().unwrap().join("staged.bin")
    );

    std::fs::write(dir.path().join("staged.bin"), b"already here").unwrap();
    let err = canonical_new_leaf_no_symlink(dir.path().join("staged.bin"))
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn canonical_existing_file_rejects_leaf_symlink_without_following_it() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.bin");
    let link = dir.path().join("link.bin");
    std::fs::write(&target, b"target").unwrap();
    make_file_symlink(&target, &link);

    let canonical = canonical_existing_file_no_symlink(&target).await.unwrap();
    assert_eq!(canonical, target.canonicalize().unwrap());

    let err = canonical_existing_file_no_symlink(&link).await.unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn canonical_helpers_reject_symlinked_ancestors() {
    let dir = tempfile::tempdir().unwrap();
    let real_root = dir.path().join("real");
    let real_nested = real_root.join("nested");
    std::fs::create_dir_all(&real_nested).unwrap();
    let existing = real_nested.join("existing.bin");
    std::fs::write(&existing, b"existing").unwrap();
    let link_root = dir.path().join("linked");
    make_dir_symlink(&real_root, &link_root);

    let existing_err = canonical_existing_file_no_symlink(link_root.join("nested/existing.bin"))
        .await
        .unwrap_err();
    assert_eq!(existing_err.error_code(), ErrorCode::ConfigInvalid);

    let new_leaf_err = canonical_new_leaf_no_symlink(link_root.join("nested/new.bin"))
        .await
        .unwrap_err();
    assert_eq!(new_leaf_err.error_code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn observe_regular_file_reports_blake3_facts_and_rejects_non_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.bin");
    std::fs::write(&path, b"voom").unwrap();

    let facts = observe_regular_file(&path).await.unwrap();

    assert_eq!(facts.path, path.canonicalize().unwrap());
    assert_eq!(facts.size_bytes, 4);
    assert_eq!(
        facts.content_hash,
        format!("blake3:{}", blake3::hash(b"voom").to_hex())
    );
    assert!(facts.modified_at.is_some());

    let err = observe_regular_file(dir.path()).await.unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::ArtifactUnavailable);
}

#[tokio::test]
async fn observe_regular_file_reports_symlink_leaf_as_artifact_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.bin");
    let link = dir.path().join("link.bin");
    std::fs::write(&target, b"target").unwrap();
    make_file_symlink(&target, &link);

    let err = observe_regular_file(&link).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactUnavailable);
}

#[tokio::test]
async fn unique_temp_sibling_path_stays_next_to_final_path() {
    let dir = tempfile::tempdir().unwrap();
    let final_path = dir.path().join("movie.mp4");

    let first = unique_temp_sibling_path(&final_path).unwrap();
    let second = unique_temp_sibling_path(&final_path).unwrap();

    assert_eq!(first.parent(), Some(dir.path()));
    assert_eq!(second.parent(), Some(dir.path()));
    assert_ne!(first, second);
    assert!(
        first
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("movie.mp4")
    );
}

#[tokio::test]
async fn copy_regular_file_checked_copies_to_new_leaf_and_verifies_hash() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.bin");
    let destination = dir.path().join("copy.bin");
    std::fs::write(&source, b"copy me").unwrap();

    let facts = copy_regular_file_checked(&source, &destination)
        .await
        .unwrap();

    assert_eq!(facts.path, destination.canonicalize().unwrap());
    assert_eq!(facts.size_bytes, 7);
    assert_eq!(
        facts.content_hash,
        format!("blake3:{}", blake3::hash(b"copy me").to_hex())
    );
    assert_eq!(std::fs::read(&destination).unwrap(), b"copy me");
}

#[tokio::test]
async fn copy_error_before_destination_ownership_does_not_remove_concurrent_file() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.bin");
    let destination = dir.path().join("copy.bin");
    std::fs::write(&source, b"copy me").unwrap();
    std::fs::write(&destination, b"concurrent writer").unwrap();

    let err = copy_regular_file_contents(&source, &destination)
        .await
        .unwrap_err();

    assert!(matches!(err, CopyFileError::NotCreated(_)));
    assert_eq!(std::fs::read(&destination).unwrap(), b"concurrent writer");
}

#[tokio::test]
async fn copy_to_unique_temp_then_install_no_replace_installs_without_temp_leftover() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.bin");
    let final_path = dir.path().join("final.bin");
    std::fs::write(&source, b"installed").unwrap();

    let facts = copy_to_unique_temp_then_install_no_replace(&source, &final_path)
        .await
        .unwrap();

    assert_eq!(facts.path, final_path.canonicalize().unwrap());
    assert_eq!(std::fs::read(&final_path).unwrap(), b"installed");
    assert_no_temp_siblings(dir.path());
}

#[tokio::test]
async fn promote_staged_add_only_cleans_temp_after_caller_visible_failure() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staged.bin");
    let target = dir.path().join("target.bin");
    std::fs::write(&staging, b"staged bytes").unwrap();
    let expected = observe_regular_file(&staging).await.unwrap();

    let err = promote_staged_add_only(&staging, &target, &expected, &RejectBeforeInstall)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::CommitFailure);
    assert!(!target.exists());
    assert_no_temp_siblings(dir.path());
}

#[tokio::test]
async fn promote_staged_add_only_uses_no_replace_install_when_target_appears_after_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staged.bin");
    let target = dir.path().join("target.bin");
    std::fs::write(&staging, b"staged bytes").unwrap();
    let expected = observe_regular_file(&staging).await.unwrap();

    let err = promote_staged_add_only(
        &staging,
        &target,
        &expected,
        &CreateTargetBeforeInstall {
            bytes: b"concurrent writer",
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::CommitFailure);
    assert_eq!(std::fs::read(&target).unwrap(), b"concurrent writer");
    assert_no_temp_siblings(dir.path());
}

#[tokio::test]
async fn promote_staged_add_only_rejects_changed_staging_facts() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staged.bin");
    let target = dir.path().join("target.bin");
    std::fs::write(&staging, b"original").unwrap();
    let expected = observe_regular_file(&staging).await.unwrap();
    std::fs::write(&staging, b"changed").unwrap();

    let err = promote_staged_add_only(&staging, &target, &expected, &NoPromotionFailpoint)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactChecksumMismatch);
    assert!(!target.exists());
    assert_no_temp_siblings(dir.path());
}

#[tokio::test]
async fn promote_staged_add_only_returns_target_facts_after_add_only_install() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staged.bin");
    let target = dir.path().join("target.bin");
    std::fs::write(&staging, b"final bytes").unwrap();
    let expected = observe_regular_file(&staging).await.unwrap();

    let report = promote_staged_add_only(&staging, &target, &expected, &NoPromotionFailpoint)
        .await
        .unwrap();

    assert_eq!(report.staging, expected);
    assert_eq!(report.target.path, target.canonicalize().unwrap());
    assert_eq!(report.target.size_bytes, expected.size_bytes);
    assert_eq!(report.target.content_hash, expected.content_hash);
    assert!(!report.temp_path.exists());
    assert_eq!(std::fs::read(&target).unwrap(), b"final bytes");
}

struct RejectBeforeInstall;

impl PromotionFailpoint for RejectBeforeInstall {
    fn before_install(&self, _context: PromotionFailpointContext<'_>) -> Result<(), VoomError> {
        Err(VoomError::CommitFailure("injected failure".to_owned()))
    }
}

struct CreateTargetBeforeInstall {
    bytes: &'static [u8],
}

impl PromotionFailpoint for CreateTargetBeforeInstall {
    fn before_install(&self, context: PromotionFailpointContext<'_>) -> Result<(), VoomError> {
        std::fs::write(context.target_path, self.bytes).unwrap();
        Ok(())
    }
}

fn assert_no_temp_siblings(dir: &Path) {
    let temp_siblings = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".voom-tmp."))
        .collect::<Vec<_>>();
    assert_eq!(temp_siblings, Vec::<String>::new());
}

#[cfg(unix)]
fn make_file_symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn make_file_symlink(target: &Path, link: &Path) {
    std::os::windows::fs::symlink_file(target, link).unwrap();
}

#[cfg(unix)]
fn make_dir_symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn make_dir_symlink(target: &Path, link: &Path) {
    std::os::windows::fs::symlink_dir(target, link).unwrap();
}
