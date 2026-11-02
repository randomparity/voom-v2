use super::*;

fn cover() -> CoverArt {
    CoverArt {
        index: 1,
        codec: "mjpeg".to_owned(),
        filename: Some("cover.jpg".to_owned()),
        mime_type: Some("image/jpeg".to_owned()),
        title: Some("Poster".to_owned()),
    }
}

#[test]
fn cover_art_rejects_invalid_or_duplicate_indices() {
    for index in [
        serde_json::Value::Null,
        serde_json::json!(-1),
        serde_json::json!(u64::MAX),
    ] {
        let probe = serde_json::json!({"streams":[{"index":index,"codec_name":"mjpeg","disposition":{"attached_pic":1}}]});
        assert!(from_probe(&probe).is_err());
    }
    let stream =
        serde_json::json!({"index":1,"codec_name":"mjpeg","disposition":{"attached_pic":1}});
    assert!(from_probe(&serde_json::json!({"streams":[stream,stream]})).is_err());
}

#[test]
fn cover_art_support_matrix_and_generic_attachment_gate() {
    for codec in ["mjpeg", "png", "gif", "tiff", "bmp"] {
        let mut art = cover();
        art.codec = codec.to_owned();
        assert_eq!(validate(&[art.clone()], "mkv", 0).is_ok(), codec != "bmp");
        assert_eq!(
            validate(&[art], "mp4", 0).is_ok(),
            matches!(codec, "mjpeg" | "png")
        );
    }
    assert!(validate(&[cover()], "mp4", 1).is_err());
}

#[cfg(unix)]
fn executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[cfg(unix)]
#[tokio::test]
async fn cover_art_verification_refuses_count_metadata_and_byte_loss() {
    for (streams, expected) in [
        (serde_json::json!([]), "count"),
        (
            serde_json::json!([{"index":1,"codec_name":"png","disposition":{"attached_pic":1}}]),
            "metadata",
        ),
        (
            serde_json::json!([{"index":1,"codec_name":"mjpeg","disposition":{"attached_pic":1},"tags":{"filename":"cover.jpg","mimetype":"image/jpeg","title":"Changed"}}]),
            "metadata",
        ),
        (
            serde_json::json!([{"index":1,"codec_name":"mjpeg","disposition":{"attached_pic":1},"tags":{"filename":"cover.jpg","mimetype":"image/jpeg","title":"Poster"}}]),
            "bytes",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let probe = serde_json::json!({"streams":streams});
        let ffprobe = executable(
            dir.path(),
            "ffprobe",
            &format!("#!/bin/sh\ncat <<'JSON'\n{probe}\nJSON\n"),
        );
        let ffmpeg = executable(
            dir.path(),
            "ffmpeg",
            "#!/bin/sh\nfor last; do :; done\nprintf mutated! > \"$last\"\n",
        );
        let config = FfmpegConfig::new(
            ffmpeg,
            ffprobe,
            "test".to_owned(),
            std::time::Duration::from_secs(1),
        );
        let source = dir.path().join("original.bin");
        std::fs::write(&source, b"original").unwrap();
        let prepared = PreparedCovers {
            directory: tempfile::tempdir_in(dir.path()).unwrap(),
            paths: vec![source],
        };
        let error = prepared
            .verify(&config, &dir.path().join("output.mkv"), &[cover()], "mkv")
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn cover_art_failed_extraction_cleans_scratch() {
    let dir = tempfile::tempdir().unwrap();
    let ffmpeg = executable(dir.path(), "ffmpeg", "#!/bin/sh\nexit 1\n");
    let config = FfmpegConfig::new(
        ffmpeg,
        dir.path().join("ffprobe"),
        "test".to_owned(),
        std::time::Duration::from_secs(1),
    );
    assert!(
        PreparedCovers::prepare(&config, &dir.path().join("input"), dir.path(), &[cover()])
            .await
            .is_err()
    );
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("voom-cover-")
    }));
}

#[cfg(unix)]
#[tokio::test]
async fn cover_art_cancelled_extraction_cleans_scratch() {
    let dir = tempfile::tempdir().unwrap();
    let ffmpeg = executable(
        dir.path(),
        "ffmpeg",
        "#!/bin/sh\nfor last; do :; done\nprintf ready > \"$last.started\"\nexec sleep 30\n",
    );
    let config = FfmpegConfig::new(
        ffmpeg,
        dir.path().join("ffprobe"),
        "test".to_owned(),
        std::time::Duration::from_secs(30),
    );
    let staging = dir.path().to_owned();
    let task = tokio::spawn(async move {
        PreparedCovers::prepare(&config, &staging.join("input"), &staging, &[cover()]).await
    });
    let ready = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if std::fs::read_dir(dir.path())
                .unwrap()
                .any(|entry| entry.unwrap().path().join("source-0.bin.started").exists())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        ready.is_ok(),
        "extraction subprocess must signal readiness before cancellation"
    );
    task.abort();
    assert!(task.await.err().unwrap().is_cancelled());
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("voom-cover-")
    }));
}
