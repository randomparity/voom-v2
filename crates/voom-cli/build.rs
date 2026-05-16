use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-env-changed=GITHUB_REF");

    let git_root = env::var("CARGO_MANIFEST_DIR").map_or_else(
        |_| PathBuf::from("../../.git"),
        |s| PathBuf::from(s).join("../..").join(".git"),
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

    // Dirty: tracked-file mods AND untracked files both count.
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| !o.stdout.is_empty());

    println!("cargo:rustc-env=VOOM_GIT_SHA={sha}");
    println!(
        "cargo:rustc-env=VOOM_GIT_DIRTY={}",
        if dirty { "true" } else { "false" }
    );
}
