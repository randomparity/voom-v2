#![expect(
    clippy::unwrap_used,
    reason = "tests use direct assertions and bounded fixture setup"
)]
use super::*;

use std::path::PathBuf;

use voom_core::FailureClass;
use voom_worker_protocol::ExpectedFileFacts;

use crate::ffprobe::FFPROBE_BIN_ENV;

const FIXTURE_JSON: &str = include_str!("../fixtures/ffprobe/basic-mp4.json");
/// A digest of no bytes in these fixtures: a probe that answers with it read
/// nothing and echoed the expected hash.
const FOREIGN_HASH: &str =
    "blake3:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

/// Reuse-listed directories this host offers: tmpfs always, and the pinned
/// test root when it is btrfs (developer hosts). A test root outside the list
/// takes the fallback assertions in `handler_test.rs` instead.
fn reuse_dirs() -> Vec<tempfile::TempDir> {
    let tmpfs = tempfile::tempdir_in("/dev/shm").unwrap();
    assert!(
        on_reuse_listed_filesystem(std::fs::File::open(tmpfs.path()).unwrap()),
        "gate G4 needs /dev/shm on tmpfs"
    );
    let mut dirs = vec![tmpfs];
    let test_root = tempfile::tempdir().unwrap();
    if on_reuse_listed_filesystem(std::fs::File::open(test_root.path()).unwrap()) {
        dirs.push(test_root);
    }
    dirs
}

/// A fake ffprobe that runs `before_output` against the probed path (its last
/// argument), then prints the fixture JSON.
fn fake_ffprobe(dir: &Path, before_output: &str) -> FfprobeConfig {
    let json = dir.join("ffprobe.json");
    std::fs::write(&json, FIXTURE_JSON).unwrap();
    let script = dir.join("ffprobe");
    let body = format!(
        "#!/bin/sh\n\
         if [ \"${{1:-}}\" = '-version' ]; then printf 'ffprobe version guard-test\\n'; exit 0; fi\n\
         for last; do :; done\n\
         {before_output}\n\
         cat '{}'\n",
        json.display()
    );
    voom_test_support::executable::write_executable(&script, body).unwrap();
    FfprobeConfig::from_env_pairs([(FFPROBE_BIN_ENV, script.as_os_str())]).unwrap()
}

fn write_media(dir: &Path) -> PathBuf {
    let path = dir.join("media.mkv");
    std::fs::write(&path, b"interior bytes the hash leg already read").unwrap();
    path
}

/// The request the scan pump would send after hashing `path` now.
fn guarded_request(path: &Path, content_hash: &str) -> ProbeFileRequest {
    let facts = GuardFacts::from_metadata(&std::fs::metadata(path).unwrap());
    ProbeFileRequest {
        path: path.display().to_string(),
        expected: ExpectedFileFacts {
            size_bytes: facts.size_bytes,
            content_hash: content_hash.to_owned(),
            modified_at: facts.modified_at,
            local_file_key: None,
        },
        guard: Some(ProbeGuard {
            changed_at_ns: facts.changed_at_ns,
            file_key: facts.file_key,
        }),
    }
}

/// Rewrite one interior byte in place (same size), then put mtime back. The
/// 20 ms wait keeps the edit out of the guard's ctime tick on coarse-clock
/// kernels, as the #798 oracle did.
fn same_size_edit_with_mtime_restored(path: &Path) {
    use std::io::{Seek, SeekFrom, Write};

    let original_mtime = std::fs::metadata(path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(3)).unwrap();
    file.write_all(b"X").unwrap();
    file.set_modified(original_mtime).unwrap();
}

async fn probe(
    request: &ProbeFileRequest,
    config: &FfprobeConfig,
) -> Result<Option<ProbeFileResult>, FfprobeError> {
    let guard = request.guard.unwrap();
    Box::pin(probe_reused(request, &guard, config)).await
}

/// Contract 5: a guard failure names its stage and nothing else.
fn assert_guard_failure(
    result: Result<Option<ProbeFileResult>, FfprobeError>,
    stage: &str,
    dir: &Path,
) {
    let err = result.unwrap_err();
    assert_eq!(err.failure_class(), FailureClass::ArtifactChecksumMismatch);
    assert_eq!(err.payload(), serde_json::json!({ "stage": stage }));
    let message = err.to_string();
    assert!(message.contains(stage), "{message}");
    assert!(!message.contains(&dir.display().to_string()), "{message}");
}

#[test]
fn reuse_list_is_btrfs_and_tmpfs_only() {
    assert!(reuse_listed(0x9123_683E), "btrfs");
    assert!(reuse_listed(0x0102_1994), "tmpfs");
    for unlisted in [0xEF53, 0x6969, 0xFE53_4D42, 0xFF53_4D42, 0x6573_5546, 0] {
        assert!(!reuse_listed(unlisted), "{unlisted:#x}");
    }
}

#[tokio::test]
async fn guarded_probe_reuses_hash_without_reading_bytes() {
    for dir in reuse_dirs() {
        let media = write_media(dir.path());
        let config = fake_ffprobe(dir.path(), ":");
        let request = guarded_request(&media, FOREIGN_HASH);

        let result = probe(&request, &config).await.unwrap().unwrap();

        assert_eq!(result.hash_source, Some(HashSource::Reused));
        for observed in [&result.pre_probe, &result.post_probe] {
            assert_eq!(observed.content_hash, FOREIGN_HASH);
            assert_eq!(observed.size_bytes, request.expected.size_bytes);
            assert_eq!(observed.modified_at, request.expected.modified_at);
        }
    }
}

// G4(a): a same-size edit between hash and probe with mtime restored.
#[tokio::test]
async fn guard_detects_restored_mtime_edit_before_probe() {
    for dir in reuse_dirs() {
        let media = write_media(dir.path());
        let config = fake_ffprobe(dir.path(), ":");
        let request = guarded_request(&media, FOREIGN_HASH);

        same_size_edit_with_mtime_restored(&media);

        assert_guard_failure(
            probe(&request, &config).await,
            "pre_probe_guard",
            dir.path(),
        );
    }
}

// G4(a) during ffprobe: the edit lands after the pre check passed.
#[tokio::test]
async fn guard_detects_restored_mtime_edit_during_probe() {
    for dir in reuse_dirs() {
        let media = write_media(dir.path());
        let reference = dir.path().join("mtime-reference");
        std::fs::write(&reference, b"").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&reference)
            .unwrap()
            .set_modified(std::fs::metadata(&media).unwrap().modified().unwrap())
            .unwrap();
        let config = fake_ffprobe(
            dir.path(),
            &format!(
                "sleep 0.02\n\
                 printf X | dd of=\"$last\" bs=1 seek=3 conv=notrunc 2>/dev/null\n\
                 touch -r '{}' \"$last\"",
                reference.display()
            ),
        );
        let request = guarded_request(&media, FOREIGN_HASH);

        assert_guard_failure(
            probe(&request, &config).await,
            "post_probe_guard",
            dir.path(),
        );
        assert_eq!(
            std::fs::metadata(&media).unwrap().modified().unwrap(),
            std::fs::metadata(&reference).unwrap().modified().unwrap(),
            "the construction must have restored mtime"
        );
    }
}

// G4(c): an ordinary write that moves mtime.
#[tokio::test]
async fn guard_detects_mtime_moving_write() {
    for dir in reuse_dirs() {
        let media = write_media(dir.path());
        let config = fake_ffprobe(dir.path(), ":");
        let request = guarded_request(&media, FOREIGN_HASH);

        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&media, b"interior bytes the hash leg already reaX").unwrap();

        assert_guard_failure(
            probe(&request, &config).await,
            "pre_probe_guard",
            dir.path(),
        );
    }
}

// A different file renamed over the path keeps nothing but the path.
#[tokio::test]
async fn guard_detects_a_file_renamed_over_the_path() {
    for dir in reuse_dirs() {
        let media = write_media(dir.path());
        let config = fake_ffprobe(dir.path(), ":");
        let request = guarded_request(&media, FOREIGN_HASH);
        let replacement = dir.path().join("replacement.mkv");
        std::fs::copy(&media, &replacement).unwrap();
        std::fs::rename(&replacement, &media).unwrap();

        assert_guard_failure(
            probe(&request, &config).await,
            "pre_probe_guard",
            dir.path(),
        );
    }
}

#[tokio::test]
async fn guarded_probe_refuses_a_symlink() {
    for dir in reuse_dirs() {
        let media = write_media(dir.path());
        let link = dir.path().join("link.mkv");
        std::os::unix::fs::symlink(&media, &link).unwrap();
        let config = fake_ffprobe(dir.path(), ":");
        let mut request = guarded_request(&media, FOREIGN_HASH);
        request.path = link.display().to_string();

        let err = probe(&request, &config).await.unwrap_err();

        assert_eq!(err.failure_class(), FailureClass::ArtifactUnavailable);
    }
}
