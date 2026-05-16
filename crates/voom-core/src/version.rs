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
mod tests {
    use super::*;

    #[test]
    fn dev_build_is_not_release() {
        let v = VersionInfo::new("0.1.0-dev", "abc1234", false, "debug");
        assert!(!v.release);
        assert_eq!(v.version, "0.1.0-dev+abc1234");
    }

    #[test]
    fn tagged_build_is_release() {
        let v = VersionInfo::new("0.1.0", "def5678", false, "release");
        assert!(v.release);
        assert_eq!(v.version, "0.1.0+def5678");
    }

    #[test]
    fn dirty_tree_appends_dirty_suffix() {
        let v = VersionInfo::new("0.1.0-dev", "abc1234", true, "debug");
        assert_eq!(v.version, "0.1.0-dev+abc1234.dirty");
    }

    #[test]
    fn unknown_sha_still_renders() {
        let v = VersionInfo::new("0.1.0-dev", "unknown", false, "debug");
        assert_eq!(v.version, "0.1.0-dev+unknown");
    }
}
