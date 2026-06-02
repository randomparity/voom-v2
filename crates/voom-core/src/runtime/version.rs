use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VersionInfo {
    pub version: String,
    pub semver: String,
    pub git_sha: String,
    pub dirty: bool,
    pub release: bool,
    pub build_profile: String,
}

impl VersionInfo {
    /// Build a `VersionInfo` from raw build-script outputs.
    ///
    /// `semver` is `CARGO_PKG_VERSION` at compile time.
    /// `git_sha` is the short SHA (or "unknown" when git is unavailable).
    /// `dirty` is true when the working tree had uncommitted changes at build.
    /// `build_profile` is "debug" or "release".
    #[must_use]
    pub fn new(semver: &str, git_sha: &str, dirty: bool, build_profile: &str) -> Self {
        let release = !semver.contains('-');
        let mut version = format!("{semver}+{git_sha}");
        if dirty {
            version.push_str(".dirty");
        }
        Self {
            version,
            semver: semver.to_owned(),
            git_sha: git_sha.to_owned(),
            dirty,
            release,
            build_profile: build_profile.to_owned(),
        }
    }
}

#[cfg(test)]
#[path = "version_test.rs"]
mod tests;
