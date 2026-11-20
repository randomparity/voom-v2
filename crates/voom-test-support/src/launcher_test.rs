use super::*;

#[test]
fn waits_cover_every_enclosed_launcher_command() {
    assert_eq!(LAUNCHER_COMMAND_TIMEOUT, Duration::from_secs(90));
    assert_eq!(WORKER_PREFLIGHT_TIMEOUT, LAUNCHER_COMMAND_TIMEOUT * 5);
    assert_eq!(AUDIO_OPERATION_TIMEOUT, LAUNCHER_COMMAND_TIMEOUT * 8);
}
