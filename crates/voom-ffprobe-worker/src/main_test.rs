use std::os::unix::fs::PermissionsExt;

use voom_core::WorkerId;
use voom_ffprobe_worker::FFPROBE_BIN_ENV;
use voom_worker_protocol::WorkerCredentials;

use super::*;

#[test]
fn worker_server_uses_supplied_credentials() {
    let bearer_fixture = "test-bearer".to_owned();
    let credentials = WorkerCredentials {
        worker_id: WorkerId(7),
        worker_epoch: 11,
        secret: bearer_fixture.into(),
    };
    let Some(config) = test_config() else {
        return;
    };

    let server = worker_server(credentials, config);

    assert_eq!(server.credentials.worker_id, WorkerId(7));
    assert_eq!(server.credentials.worker_epoch, 11);
}

fn test_config() -> Option<FfprobeConfig> {
    let Ok(dir) = tempfile::tempdir() else {
        return None;
    };
    let path = dir.path().join("ffprobe");
    assert!(
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf 'ffprobe version test-helper Copyright\\n'\n"
        )
        .is_ok()
    );
    let Ok(metadata) = std::fs::metadata(&path) else {
        return None;
    };
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    assert!(std::fs::set_permissions(&path, permissions).is_ok());
    let result = FfprobeConfig::from_env_pairs([(FFPROBE_BIN_ENV, path.as_os_str())]);
    assert!(result.is_ok());
    result.ok()
}
