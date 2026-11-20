//! Bounds a test harness gives the containerised media launcher
//! (`containers/ffmpeg/runtime.py`), derived from the launcher's own budgets so a harness
//! wait never expires before the launcher reports which engine command overran.

use std::time::Duration;

/// Seconds for one launcher command: the launcher's 60 s operation timeout (30 s setup
/// budget, 15 s cleanup budget, 15 s launch margin) plus 30 s for a short media command.
/// Equals `HARNESS_COMMAND_TIMEOUT` in `containers/ffmpeg/runtime.py`; `runtime_test.py`
/// pins the two together.
pub const LAUNCHER_COMMAND_SECONDS: u64 = 90;

/// One launcher command, such as a probe or a fixture encode.
pub const LAUNCHER_COMMAND_TIMEOUT: Duration = Duration::from_secs(LAUNCHER_COMMAND_SECONDS);

/// Worker readiness: ffmpeg-worker preflight runs five launcher commands before it binds
/// (`ffmpeg -version`, `ffprobe -version`, `-encoders`, `-h muxer=matroska`, `-muxers`).
pub const WORKER_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(5 * LAUNCHER_COMMAND_SECONDS);

/// One audio operation runs at most eight launcher commands: input probe, encode, output
/// probe, compatibility probe, and the copied-audio source/output probes and packet scans.
pub const AUDIO_OPERATION_TIMEOUT: Duration = Duration::from_secs(8 * LAUNCHER_COMMAND_SECONDS);
