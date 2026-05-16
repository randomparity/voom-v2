# Release Process

VOOM follows the bump → tag → bump cadence so `main` always carries a `-dev`
SemVer suffix between releases. The release process is run from `main`.

## Steps

1. **Bump to the release version.** On `main`, edit the workspace
   `Cargo.toml`'s `[workspace.package] version` from `0.X.Y-dev` to `0.X.Y`.
   Run `cargo build` to refresh `Cargo.lock`, then commit:

   ```bash
   git add Cargo.toml Cargo.lock
   git commit -m "Release: 0.X.Y"
   ```

2. **Tag the release commit.**

   ```bash
   git tag -a v0.X.Y -m "voom 0.X.Y"
   git push origin v0.X.Y
   ```

   The `release.yml` workflow builds linux-x64, linux-arm64, and macos-arm64
   binaries on tag push and uploads them to a draft GitHub Release.

3. **Bump to the next dev version.** Immediately on `main`, bump
   `[workspace.package] version` from `0.X.Y` → `0.X.(Y+1)-dev` (patch) or
   `0.(X+1).0-dev` (minor). Run `cargo build`, then commit:

   ```bash
   git add Cargo.toml Cargo.lock
   git commit -m "Begin 0.X.(Y+1)-dev"
   ```

4. **Publish the GitHub Release.** Edit the draft, paste a changelog (or
   `git log v0.X.(Y-1)..v0.X.Y --oneline`), and publish. The release artifacts
   self-report version as `0.X.Y+<tag-sha>`.

   Build-script provenance smoke check (run once per release candidate): build
   the binary, commit an empty change (`git commit --allow-empty`), build
   again, run `voom version`, and confirm the reported SHA advanced.

## Never

- Amend tags after creation.
- Force-push to `main`.
- Skip the post-release bump commit (otherwise the next `main` build reports
  the released version, breaking `--release` provenance).
