use std::io;
use std::process::Command;

use voom_core::VersionInfo;

use crate::envelope::emit_ok;

/// Probe `git status --porcelain` at runtime and return the current dirty
/// state. Returns `None` when git is unavailable, the cwd isn't a checkout,
/// or the command fails — callers fall back to the compile-time flag in that
/// case.
fn probe_dirty_at_runtime() -> Option<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(!output.stdout.is_empty())
}

pub fn run() -> io::Result<()> {
    let semver = env!("CARGO_PKG_VERSION");
    let sha = env!("VOOM_GIT_SHA");
    let compile_time_dirty = matches!(env!("VOOM_GIT_DIRTY"), "true");
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    // Debug builds re-probe dirty at runtime so an edit between builds is
    // reflected even when Cargo reuses the cached build-script output. Release
    // builds trust the compile-time flag because they ship without a git
    // checkout and are built from clean CI tags (see docs/release-process.md).
    let dirty = if cfg!(debug_assertions) {
        probe_dirty_at_runtime().unwrap_or(compile_time_dirty)
    } else {
        compile_time_dirty
    };

    let info = VersionInfo::new(semver, sha, dirty, profile);
    emit_ok("version", info, None, Vec::new())
}
