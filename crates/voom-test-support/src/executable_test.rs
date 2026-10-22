#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions express test contracts; Result propagates fixture setup failures"
)]

use super::*;

#[cfg(unix)]
#[test]
fn fixture_bytes_and_quoted_path_execute_immediately() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir()?;
    let path = directory
        .path()
        .join("fixture ' $(touch unexpected) ; \"name\"");
    let body = b"#!/bin/sh\nprintf '%s' 'fixture data'\n";
    write_executable(&path, body)?;
    assert_eq!(std::fs::read(&path)?, body);
    assert_eq!(
        std::fs::metadata(&path)?.permissions().mode() & 0o777,
        0o755
    );
    let output = std::process::Command::new(&path).output()?;
    assert!(output.status.success());
    assert_eq!(output.stdout, b"fixture data");
    assert_eq!(directory.path().read_dir()?.count(), 1);
    Ok(())
}

#[test]
fn fixture_writer_preserves_large_binary_content() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("binary");
    let bytes: Vec<u8> = (0_u8..=255).cycle().take(1024 * 1024).collect();
    write_executable(&path, &bytes)?;
    assert_eq!(std::fs::read(path)?, bytes);
    Ok(())
}

#[test]
fn fixture_writer_surfaces_invalid_destination() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let missing = directory.path().join("missing").join("fixture");
    assert!(write_executable(&missing, b"#!/bin/sh\nexit 0\n").is_err());
    assert!(!missing.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn parallel_fixture_writers_and_consumers_need_no_spawn_retry() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    std::thread::scope(|scope| {
        let threads: Vec<_> = (0..8)
            .map(|index| {
                let root = directory.path();
                scope.spawn(move || -> io::Result<()> {
                    for iteration in 0..8 {
                        let path = root.join(format!("fixture-{index}-{iteration}"));
                        write_executable(&path, b"#!/bin/sh\nexit 0\n")?;
                        assert!(std::process::Command::new(path).status()?.success());
                    }
                    Ok(())
                })
            })
            .collect();
        for thread in threads {
            thread
                .join()
                .map_err(|_| io::Error::other("fixture thread panicked"))??;
        }
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn fixture_writer_rejects_long_invalid_destination_without_hanging() -> io::Result<()> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const PROBE: &str = "VOOM_EXECUTABLE_WRITER_ERROR_PROBE";
    if std::env::var_os(PROBE).is_some() {
        let path = std::path::PathBuf::from("x".repeat(100_000));
        assert!(write_executable(&path, vec![0_u8; 1024 * 1024]).is_err());
        return Ok(());
    }
    let mut child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "executable::tests::fixture_writer_rejects_long_invalid_destination_without_hanging",
        ])
        .env(PROBE, "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                break Err(io::Error::other(
                    "fixture writer did not return a destination error",
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => break Err(error),
        }
    };
    if result.is_err() {
        child.kill()?;
        child.wait()?;
    }
    assert!(result?.success());
    Ok(())
}
