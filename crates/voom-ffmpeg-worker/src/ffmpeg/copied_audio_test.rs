use super::*;

#[test]
fn rational_timing_preserves_exact_values_and_absence() {
    let milliseconds = TimeBase::parse("1/1000").unwrap();
    let samples = TimeBase::parse("1/48000").unwrap();
    assert_eq!(
        milliseconds.time(Some("15")).unwrap(),
        samples.time(Some("720")).unwrap()
    );
    assert_ne!(
        milliseconds.time(Some("15")).unwrap(),
        samples.time(Some("700")).unwrap()
    );
    assert_eq!(milliseconds.time(None).unwrap(), None);
    assert_ne!(
        milliseconds.time(None).unwrap(),
        milliseconds.time(Some("0")).unwrap()
    );
    assert_eq!(milliseconds.time(Some("-15")).unwrap(), Some((-3, 200)));
    for invalid in ["0/1", "1/0", "-1/1000", "1/2/3"] {
        assert!(TimeBase::parse(invalid).is_err());
    }
}

#[test]
fn compact_fields_allow_suppressed_side_data_but_reject_ambiguous_fields() {
    assert!(fields("stream_index=2|pts=0|data_hash=SHA256:00|").is_ok());
    for line in [
        "stream_index=2||pts=0",
        "stream_index=1|stream_index=2",
        "unknown=1",
    ] {
        assert!(fields(line).is_err());
    }
}

fn stream(index: u64) -> Stream {
    Stream::new(&serde_json::json!({"index":index,"time_base":"1/1000"})).unwrap()
}

fn record(index: u64, duration: Option<i64>) -> String {
    format!(
        "stream_index={index}|pts=-7|dts=-7|{}data_hash=SHA256:{}\n",
        duration.map_or_else(String::new, |value| format!("duration={value}|")),
        "00".repeat(32)
    )
}

#[tokio::test]
async fn packet_digest_proves_order_count_timing_and_missing_duration() {
    let input = record(2, Some(15)) + &record(2, Some(20));
    let original = packets(input.as_bytes(), vec![stream(2)], BTreeSet::from([2]))
        .await
        .unwrap();
    assert_eq!(original[0].0, 2);
    for changed in [
        record(2, Some(14)) + &record(2, Some(20)),
        record(2, Some(20)) + &record(2, Some(15)),
        record(2, Some(15)),
        record(2, None) + &record(2, Some(20)),
        input.replace("SHA256:00", "SHA256:01"),
    ] {
        let actual = packets(changed.as_bytes(), vec![stream(2)], BTreeSet::from([2]))
            .await
            .unwrap();
        assert_ne!(
            original, actual,
            "changed packet observation must not prove preservation"
        );
    }
    let absent = record(2, None);
    assert_eq!(
        packets(absent.as_bytes(), vec![stream(2)], BTreeSet::from([2]))
            .await
            .unwrap(),
        packets(absent.as_bytes(), vec![stream(2)], BTreeSet::from([2]))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn packet_reader_rejects_incomplete_or_malformed_observations() {
    let valid = record(2, Some(15));
    for bad in [
        valid.trim_end().to_owned(),
        valid.replace("stream_index=2", "stream_index=3"),
        valid.replace("data_hash=", "hash="),
        valid.replace("SHA256:", "SHA1:"),
        valid.replace("pts=-7", "pts=9223372036854775808"),
        format!("stream_index=2|{}\n", "x".repeat(1024)),
    ] {
        assert!(
            packets(bad.as_bytes(), vec![stream(2)], BTreeSet::from([2]))
                .await
                .is_err(),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn packet_digest_maps_only_copied_audio_and_accepts_suppressed_opus_sections() {
    let input = record(1, Some(20)) + &record(3, Some(20)).replace('\n', "|\n");
    let observed = packets(input.as_bytes(), vec![stream(3)], BTreeSet::from([1, 3]))
        .await
        .unwrap();
    let expected = packets(
        record(3, Some(20)).as_bytes(),
        vec![stream(3)],
        BTreeSet::from([3]),
    )
    .await
    .unwrap();
    assert_eq!(observed, expected);
}

fn probe_config(directory: &Path, body: &str) -> FfmpegConfig {
    let executable = directory.join("ffprobe");
    voom_test_support::executable::write_executable(&executable, format!("#!/bin/sh\n{body}\n"))
        .unwrap();
    FfmpegConfig::new(
        "ffmpeg".into(),
        executable,
        "test".to_owned(),
        Duration::from_millis(100),
    )
}

async fn observe(config: &FfmpegConfig) -> Result<Vec<Fingerprint>, FfmpegError> {
    let metadata = serde_json::json!({"index":2,"time_base":"1/1000"});
    collect(
        config,
        Path::new("input.mkv"),
        vec![stream(2)],
        &[&metadata],
    )
    .await
}

#[tokio::test]
async fn zero_exit_probe_errors_cannot_authorize_packet_observations() {
    let directory = tempfile::tempdir().unwrap();
    let body = format!(
        "printf '%s\\n' '{}'; printf '%s\\n' '/private/media/input.mkv: File ended prematurely' >&2",
        record(2, Some(15)).trim_end()
    );
    let mut config = probe_config(directory.path(), &body);
    config.process_timeout = Duration::from_secs(2);
    let error = observe(&config).await.unwrap_err().to_string();
    assert!(error.contains("packet probe reported errors"), "{error}");
    assert!(
        !error.contains("/private/media"),
        "diagnostics must be sanitized"
    );
}

#[tokio::test]
async fn packet_probe_bounds_both_pipes_and_propagates_nonzero_exit() {
    for (body, expected) in [
        (
            "i=0; while [ $i -lt 2000 ]; do printf x; i=$((i+1)); done",
            "oversized",
        ),
        (
            "i=0; while [ $i -lt 20000 ]; do printf x >&2; i=$((i+1)); done",
            "diagnostic limit",
        ),
        ("exit 7", "exited unsuccessfully"),
        ("exec sleep 20", "timed out"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut config = probe_config(directory.path(), body);
        config.process_timeout = Duration::from_secs(2);
        let started = std::time::Instant::now();
        let error = observe(&config).await.unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "probe must remain bounded"
        );
    }
}

#[tokio::test]
async fn packet_probe_streams_more_than_pipe_capacity_without_retaining_packets() {
    let directory = tempfile::tempdir().unwrap();
    let body = format!(
        "i=0; while [ $i -lt 4000 ]; do printf '%s\\n' '{}'; i=$((i+1)); done",
        record(2, Some(20)).trim_end()
    );
    let mut config = probe_config(directory.path(), &body);
    config.process_timeout = Duration::from_secs(10);
    assert_eq!(observe(&config).await.unwrap()[0].0, 4000);
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_packet_probe_kills_and_reaps_the_child() {
    let directory = tempfile::tempdir().unwrap();
    let pid_path = directory.path().join("pid");
    let config = probe_config(
        directory.path(),
        &format!(
            "printf '%s' \"$$\" > '{}'; exec sleep 20",
            pid_path.display()
        ),
    );
    let task = tokio::spawn(async move { observe(&config).await });
    let pid = timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(value) = tokio::fs::read_to_string(&pid_path).await
                && let Ok(pid) = value.parse::<i32>()
            {
                break pid;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let reaped = timeout(Duration::from_secs(3), async {
        loop {
            if !std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()
                .unwrap()
                .status
                .success()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(reaped.is_ok(), "cancelled probe was not reaped");
}

#[tokio::test]
async fn missing_source_streams_cannot_establish_no_copied_audio() {
    let directory = tempfile::tempdir().unwrap();
    let config = probe_config(directory.path(), "printf '%s' '{\"streams\":[]}'");
    let selected = [AudioStreamRef {
        snapshot_stream_id: "selected".to_owned(),
        provider_stream_index: 1,
    }];
    assert!(
        verify(
            &config,
            Path::new("input.mkv"),
            Path::new("out.mkv"),
            &selected,
            false
        )
        .await
        .is_err()
    );
}
