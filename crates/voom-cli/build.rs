use std::env;
use std::path::PathBuf;
use std::process::Command;

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

    // Watch the workspace's crate sources recursively. Without this, edits to
    // tracked files (in any crate) leave `git status --porcelain` dirty but
    // don't touch the build script's existing watched inputs, so cargo
    // happily reuses a cached `VOOM_GIT_DIRTY=false` and a released binary
    // can lie about provenance.
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("crates").display()
    );
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

    // If HEAD is a symbolic ref, also watch the file backing the current branch.
    if let Ok(out) = Command::new("git")
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()
        && out.status.success()
    {
        let r = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        println!("cargo:rerun-if-changed={}", git_root.join(&r).display());
    }

    // SHA: prefer CI-provided env, fall back to `git rev-parse`.
    let sha = env::var("GITHUB_SHA")
        .ok()
        .map(|s| s.chars().take(7).collect::<String>())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".to_owned());

    // Dirty flag resolution order:
    //   1. `VOOM_GIT_DIRTY` env (set by CI/release scripts after probing
    //      git status — authoritative, bypasses cargo cache concerns).
    //   2. `git status --porcelain` at build time (tracked + untracked
    //      files both count for provenance).
    //   3. Default false when git is unavailable (shipped binaries).
    let dirty = env::var("VOOM_GIT_DIRTY").ok().map_or_else(
        || {
            Command::new("git")
                .args(["status", "--porcelain"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .is_some_and(|o| !o.stdout.is_empty())
        },
        |v| v == "true",
    );

    println!("cargo:rustc-env=VOOM_GIT_SHA={sha}");
    println!(
        "cargo:rustc-env=VOOM_GIT_DIRTY={}",
        if dirty { "true" } else { "false" }
    );
}
