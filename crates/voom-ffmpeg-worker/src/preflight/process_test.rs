use std::{
    io,
    process::{Command, Stdio},
    time::Duration,
};

use super::*;

#[test]
fn probe_directories_are_unique_when_created_concurrently() {
    const THREADS: usize = 64;

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
    let handles = (0..THREADS)
        .map(|_| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                ProbeDir::new("concurrent-probe").map(|probe| probe.path().to_owned())
            })
        })
        .collect::<Vec<_>>();

    let mut paths = handles
        .into_iter()
        .map(|handle| handle.join().unwrap().unwrap())
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths.dedup();

    assert_eq!(paths.len(), THREADS);
}

#[test]
fn command_output_drains_a_child_that_outwrites_the_pipe_capacity() {
    let mut chatty = Command::new("/bin/sh");
    chatty.args([
        "-c",
        "awk 'BEGIN { while (n++ < 8192) print \
         \"0123456789abcdef0123456789abcde\" }'",
    ]);

    let output = command_output(&mut chatty).unwrap();

    assert!(output.status.success());
    assert_eq!(output.stdout.len(), 8192 * 32);
}

/// Draining must not defeat the deadline. The stub leaves a `sleep` grandchild
/// holding the same pipe, so killing the direct child does not close it — waiting on
/// the drain would block for the grandchild's whole lifetime, which is precisely the
/// startup hang the deadline exists to prevent. The elapsed assertion is the point of
/// this test: returning `TimedOut` eventually is not the same as returning promptly.
#[cfg(unix)]
#[test]
fn wait_child_output_times_out_promptly_when_a_grandchild_holds_the_pipe() {
    let child = Command::new("/bin/sh")
        .args(["-c", "sleep 60"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let started = std::time::Instant::now();
    let error = wait_child_output_io(child, Duration::from_millis(200), "stuck probe").unwrap_err();
    let elapsed = started.elapsed();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("stuck probe exceeded"));
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
}
