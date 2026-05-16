use std::io;

use voom_core::VersionInfo;

use crate::envelope::emit_ok;

#[must_use]
pub fn build_version_info(
    semver: &str,
    git_sha: &str,
    dirty: bool,
    build_profile: &str,
) -> VersionInfo {
    VersionInfo::new(semver, git_sha, dirty, build_profile)
}

pub fn run() -> io::Result<()> {
    let semver = env!("CARGO_PKG_VERSION");
    let sha = env!("VOOM_GIT_SHA");
    let dirty = matches!(env!("VOOM_GIT_DIRTY"), "true");
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    let info = build_version_info(semver, sha, dirty, profile);
    emit_ok("version", info, None, Vec::new())
}
