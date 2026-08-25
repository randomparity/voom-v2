# 0079 — Deterministic test temp root

## Status

Accepted (2026-08-25)

## Context

`tempfile` is called directly in 72 test files, and `TMPDIR` was set nowhere in
the repo, so every `SQLite` test inherited whatever `/tmp` happened to be on the
host running it:

- Fedora and Ubuntu 24.04+ workstations mount `/tmp` as **tmpfs**, where `fsync`
  is nearly free;
- CI runners and macOS (`/var/folders`) use a **real disk**, where `fsync` costs
  milliseconds.

In this suite `fsync` duration is how long a `SQLite` write lock is held, which
is the quantity that decides `SQLITE_BUSY`, lease expiry and watchdog races. So
a Linux developer was exercising materially different timing from CI and from a
colleague on macOS, with nothing at the call site to say so. Four independent
flakes surfaced in one session (#541, #542, #543, #545, #546), and "cannot
reproduce locally" was recorded more than once against a suite that was, in
fact, running on different storage.

Two further facts shaped the decision. `tempfile` fails outright when `TMPDIR`
names a directory that does not exist — verified: it returns
`NotFound` naming the path — so the location cannot be created lazily on first
use. And macOS sets `TMPDIR` from launchd, as do some Linux distributions, so an
inherited value is the common case rather than the exception.

## Decision

Pin the test temp root to `.test-tmp/` at the workspace root, via an `[env]`
entry in `.cargo/config.toml` with `force = true`.

Cargo applies `[env]` to every process it runs, so a bare `cargo test` is
covered and not only the `just` recipes. `force = true` overrides an inherited
`TMPDIR`, because uniformity is the entire point: an inherited value silently
reintroduces the divergence being removed.

The directory is kept alive by a tracked self-ignoring `.gitignore` (`*` plus
`!.gitignore`), so it exists on a fresh clone before anything runs.

`temp_databases_land_on_the_pinned_repo_local_root` asserts a `TempDatabase`
lands under that root and names `.cargo/config.toml` in its failure message.
Verified to bite: removing the config reddens it with the offending path.

## Consequences

- Every host runs these tests on real storage, so timing behaviour is
  comparable between a workstation, a runner, and macOS.
- Bare `cargo test` gets the same environment as `just test`; there is no
  sanctioned path that skips it.
- `.test-tmp/` is not cleaned by `cargo clean`. `TempDir` removes its own
  directories on drop, so this only accumulates when a test process is killed
  hard. No recipe is provided to clear it until that proves to be a real
  annoyance.
- Tests now write into the repo working tree rather than `/tmp`. The contents
  are git-ignored, so `rg` and `cargo` skip them.
- Anything genuinely needing the system temp directory must ask for it
  explicitly rather than relying on the ambient default.

## Considered & rejected

- **Set `TMPDIR` only in the `just` test recipes.** judgment: leaves bare
  `cargo test` on the old behaviour, which is the command developers reach for
  when narrowing a single failure — exactly when the timing difference matters
  most.
- **Change `TempDatabase` to resolve its own root.** verified: `tempfile` is
  called directly in 72 files (`rg -l "tempfile::(tempdir|TempDir|NamedTempFile|Builder)"`,
  this workspace at `84a30e47`), so routing one helper would leave most call
  sites on the ambient default while appearing to solve the problem.
- **`[env]` without `force = true`.** verified: cargo does not override an
  already-set variable unless forced, and macOS always sets `TMPDIR` from
  launchd — so the pin would silently not apply on the platform half of the CI
  matrix.
- **Point `TMPDIR` at `target/`.** judgment: it always exists and is already
  ignored, but it mixes ephemeral test state into the build-artifact tree, and
  a `CARGO_TARGET_DIR` override moves it somewhere unrelated to the checkout.
- **Create the directory from a build script.** judgment: adds a build script,
  and its ordering guarantee only covers crates that depend on it — the
  problem is workspace-wide.
- **Leave it and document the difference in `AGENTS.md`.** judgment: the failure
  mode is a wrong conclusion ("does not reproduce locally") drawn from a silent
  difference, and a note in a file nobody re-reads mid-investigation does not
  prevent it.
