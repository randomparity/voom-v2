#[cfg(not(test))]
use std::{env, path::PathBuf, process::Command};

#[cfg(not(test))]
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-env-changed=GITHUB_REF");
    // Allow CI/release scripts to set VOOM_GIT_DIRTY explicitly. Honored
    // unconditionally below; rerun-if-env-changed ensures cargo re-runs the
    // script when the env value flips.
    println!("cargo:rerun-if-env-changed=VOOM_GIT_DIRTY");

    let manifest_dir =
        env::var("CARGO_MANIFEST_DIR").map_or_else(|_| PathBuf::from("."), PathBuf::from);
    let workspace_root = manifest_dir.join("../..");
    let git_root = workspace_root.join(".git");

    // The recursive `crates/` watch makes every edit anywhere invalidate this
    // crate's build-script cache, which defeats incremental builds. Only
    // release builds need this level of provenance accuracy — debug builds
    // re-probe dirty at runtime (see `commands/version.rs`). Skipping the
    // watch in dev keeps cross-crate edits cheap.
    let is_release = env::var("PROFILE").as_deref() == Ok("release");
    if is_release {
        println!(
            "cargo:rerun-if-changed={}",
            workspace_root.join("crates").display()
        );
    }
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("migrations").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("Cargo.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("Cargo.lock").display()
    );

    // Watch HEAD and packed-refs unconditionally.
    println!("cargo:rerun-if-changed={}", git_root.join("HEAD").display());
    println!(
        "cargo:rerun-if-changed={}",
        git_root.join("packed-refs").display()
    );

    // Single `git status -b --porcelain=v2` gives us SHA (`# branch.oid`),
    // branch (`# branch.head`), and dirty (any non-`#` line) in one fork —
    // replacing the previous trio of `symbolic-ref` + `rev-parse` + `status`.
    let git_status = read_git_status();

    // If on a branch (not detached), also watch the file backing that
    // branch's ref so a checkout/commit invalidates the build-script cache.
    if let Some(branch) = git_status.as_ref().and_then(|s| s.branch.as_deref()) {
        println!(
            "cargo:rerun-if-changed={}",
            git_root.join("refs/heads").join(branch).display()
        );
    }

    // SHA: prefer CI-provided env, fall back to the parsed `git status` line.
    let sha = env::var("GITHUB_SHA")
        .ok()
        .map(|s| s.chars().take(7).collect::<String>())
        .or_else(|| {
            git_status
                .as_ref()
                .and_then(|s| s.sha.as_ref())
                .map(|s| s.chars().take(7).collect::<String>())
        })
        .unwrap_or_else(|| "unknown".to_owned());

    // Dirty flag resolution order:
    //   1. `VOOM_GIT_DIRTY` env (set by CI/release scripts — authoritative).
    //   2. The parsed `git status` we already ran.
    //   3. Default false when git is unavailable (shipped binaries).
    let dirty = env::var("VOOM_GIT_DIRTY")
        .ok()
        .map_or_else(|| git_status.is_some_and(|s| s.dirty), |v| v == "true");

    println!("cargo:rustc-env=VOOM_GIT_SHA={sha}");
    println!(
        "cargo:rustc-env=VOOM_GIT_DIRTY={}",
        if dirty { "true" } else { "false" }
    );
}

#[derive(Debug)]
pub(crate) struct GitStatus {
    pub(crate) sha: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) dirty: bool,
}

/// Run `git status -b --porcelain=v2` once and extract SHA, branch, and
/// dirty-flag in a single fork. Returns `None` when git is unavailable or
/// the cwd isn't a checkout.
#[cfg(not(test))]
fn read_git_status() -> Option<GitStatus> {
    let out = Command::new("git")
        .args(["status", "-b", "--porcelain=v2"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    parse_git_status(&stdout)
}

pub(crate) fn parse_git_status(stdout: &str) -> Option<GitStatus> {
    let mut saw_status_line = false;
    let mut sha = None;
    let mut branch = None;
    let mut dirty = false;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("# branch.oid ") {
            saw_status_line = true;
            let s = rest.trim();
            // `initial` appears before the first commit; treat as no SHA.
            if s != "(initial)" {
                sha = Some(s.to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("# branch.head ") {
            saw_status_line = true;
            let b = rest.trim();
            if b != "(detached)" {
                branch = Some(b.to_owned());
            }
        } else if !line.is_empty() && !line.starts_with('#') {
            // Any non-comment line = a tracked or untracked entry = dirty.
            saw_status_line = true;
            dirty = true;
        }
    }
    saw_status_line.then_some(GitStatus { sha, branch, dirty })
}
