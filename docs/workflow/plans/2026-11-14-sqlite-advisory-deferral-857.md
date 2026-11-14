# Bundled SQLite advisory deferral, quinn-udp and rustix bumps — plan (#857, #858, #861)

**Goal:** make `just platform-checks` and the `audit` workflow green by deferring
RUSTSEC-2026-0459 under a recorded, time-boxed ignore and fixing RUSTSEC-2026-0538 and
RUSTSEC-2026-0486 with lockfile bumps.

**Architecture:** configuration and documentation only. Two advisory-policy files gain one
narrow ignore each, a `docs/debt/` record owns the exception, and `Cargo.lock` moves two crates.

**Tech stack:** cargo 1.95.0, cargo-deny 0.19.0, cargo-audit 0.22.2, just, prek.

Spec: `docs/workflow/specs/2026-11-14-sqlite-advisory-deferral-857-design.md`.

Expected implementation size: 100–140 changed lines (M) — the debt record (~80 lines), two
policy-file edits (~15 lines), and two lockfile entries (~8 lines).

## Global Constraints

- Ignore exactly `RUSTSEC-2026-0459`. Never ignore `RUSTSEC-2026-0538` or `RUSTSEC-2026-0486`.
- Do not change sqlx, `libsqlite3-sys`, any `Cargo.toml`, any workflow, or any source file.
- `Cargo.lock` changes only the `version`/`checksum` lines of `quinn-udp` 0.5.15 -> 0.5.16 and
  `rustix` 1.1.4 -> 1.1.6 (both resolved on crates.io, not yanked). `cargo update` also
  rewires `winapi-util`'s `cfg(windows)` edge to `windows-sys` 0.48.0; restore every such
  non-target hunk to its base value.
- If any other advisory appears, or `cargo deny check bans` fails after the restore, stop and
  report; do not widen and add no `[bans]` skip.
- Debt record number is `0008` (assigned); review-by `2027-02-14`.
- Lines <= 100 chars; conventional commits; never `--no-verify`. Run commits with a raised
  timeout and check `git status` after each (prek can drop unstaged files on timeout).

## File map

| File | Owns today | Change |
|---|---|---|
| `docs/debt/0008-bundled-sqlite-advisory-awaits-an-sqlx-release.md` | new | the deferral |
| `deny.toml` | cargo-deny policy | one reasoned ignore |
| `.cargo/audit.toml` | cargo-audit policy (also read by the `audit` workflow) | one ignore + comment |
| `Cargo.lock` | resolved graph | quinn-udp 0.5.16, rustix 1.1.6 |

Task order is forced by the prek hooks: Task 1 touches `deny.toml` (runs `just deny`, which
flags only RUSTSEC-2026-0459); Task 2 touches `Cargo.lock` (runs `just deny` and `just audit`).

## Task 1 — Defer RUSTSEC-2026-0459 behind a recorded ignore

**Interfaces:** produces the debt record path both ignores name. Task 2 relies on
RUSTSEC-2026-0459 being ignored by both tools.

**Verification:**
- Contract: deny ignore is load-bearing. `Mode: focused-test` — red: `cargo deny check
  advisories` fails naming `RUSTSEC-2026-0459`; green: `just deny` exits 0.
- Contract: audit ignore is load-bearing. `Mode: focused-test` — red: `cargo audit --deny
  warnings 2>&1 | rg RUSTSEC-2026-0459` matches; green: same command matches nothing (the run
  still fails on quinn-udp/rustix until Task 2).
- Contract: debt record content. `Mode: task-test-not-applicable` — no executable consumer
  validates `docs/debt/` (no gate in `justfile`, `scripts/`, `.github/`, prek config).

Steps:
1. Run `cargo deny check advisories` — expect failure on RUSTSEC-2026-0459 only (red).
2. Write `docs/debt/0008-bundled-sqlite-advisory-awaits-an-sqlx-release.md` with sections
   `Status` (`Open`, `review-by: 2027-02-14`), `Concern`, `Exposure`, `Why deferred`,
   `Non-regression boundary`, `What would resolve it`, `Provenance` — content per the spec's
   Scope section, citing `crates/voom-store/Cargo.toml`, `crates/voom-store/src/pool.rs`
   (`connect`, `connect_or_create`), `crates/voom-core/src/runtime/config.rs`
   (`default_database_url`), the crates.io requirements of `sqlx-sqlite` 0.8.6 (`^0.30.1`) and
   0.9.0 (`>=0.30.1, <0.38.0`), and `tracker: #857`. Bound the exposure claim to current code
   paths at `818cc91a`.
3. In `deny.toml` replace `ignore = []` with:

   ```toml
   ignore = [
       # Bundled SQLite CVE-2026-119375; no released sqlx permits the fixed
       # libsqlite3-sys >= 0.39. Remove with the bump; see docs/debt/0008.
       { id = "RUSTSEC-2026-0459", reason = "awaiting sqlx release; docs/debt/0008" },
   ]
   ```

4. In `.cargo/audit.toml` add a `# RUSTSEC-2026-0459 — ...` comment block after the
   RUSTSEC-2023-0071 block (cause, why not fixable, exit condition, record path, and that the
   `audit` workflow reads this file) and set
   `ignore = ["RUSTSEC-2023-0071", "RUSTSEC-2026-0459"]`.
5. `just deny` — expect exit 0. `cargo audit --deny warnings 2>&1 | rg RUSTSEC-2026-0459` —
   expect no match.
6. Load-bearing check: remove the entry from `deny.toml`, run `just deny`, expect failure
   naming RUSTSEC-2026-0459, restore; remove the ID from `.cargo/audit.toml`, rerun the step 5
   `rg` command, expect a match, restore. `git diff` must equal the step 3–4 edits.
7. Commit: `fix(deps): defer RUSTSEC-2026-0459 until sqlx permits the fix`; then
   `git status --short` — expect empty.

## Task 2 — Fix RUSTSEC-2026-0538 and RUSTSEC-2026-0486 by lockfile bumps

**Interfaces:** consumes Task 1's ignores; produces a lockfile with no unignored advisory.

**Verification:**
- Contract: no advisory for quinn-udp or rustix. `Mode: focused-test` — red: `cargo audit
  --deny warnings` exits 1 listing RUSTSEC-2026-0538 and RUSTSEC-2026-0486; green: `just audit`
  exits 0.
- Contract: workflow invocation is clean. `Mode: focused-test` — `cargo audit --json --file
  ./Cargo.lock | jq .vulnerabilities.found` from the repo root prints `false`.
- Contract: lockfile scope. `Mode: focused-test` — red: after the two `cargo update` runs,
  `cargo deny check bans` fails on duplicate `windows-sys`; green: after the restore,
  `git diff -U0 -- Cargo.lock` shows only the quinn-udp and rustix `version`/`checksum`
  lines, `cargo metadata --locked` exits 0, and `cargo deny check bans` exits 0.

Steps:
1. `cargo audit --deny warnings` — expect exit 1 with RUSTSEC-2026-0538 and
   RUSTSEC-2026-0486 only (red).
2. `cargo update -p quinn-udp --precise 0.5.16` — expect `Updating quinn-udp v0.5.15 -> v0.5.16`.
3. `cargo update -p rustix --precise 1.1.6` — expect `Updating rustix v1.1.4 -> v1.1.6`.
4. `git diff -U0 -- Cargo.lock` — expect the two `version`/`checksum` pairs plus one hunk in
   the `winapi-util` 0.1.11 entry changing `"windows-sys 0.61.2"` to `"windows-sys 0.48.0"`.
   Edit that line back to `"windows-sys 0.61.2"`; restore any other non-target hunk the same
   way. Then `git diff --stat -- Cargo.lock` — expect
   `1 file changed, 4 insertions(+), 4 deletions(-)`.
5. `cargo metadata --locked --format-version 1 >/dev/null` — expect exit 0 (cargo accepts the
   lockfile without re-resolving). `cargo fetch --locked` — expect exit 0 (checksums match).
6. `just audit` and `just deny` — expect exit 0 each; run the JSON check — expect `false`.
   A `bans` failure here is a stop, not a skip.
7. Commit: `fix(deps): bump quinn-udp and rustix for RUSTSEC-2026-0538/0486`; then
   `git status --short` — expect empty.

## Final guardrails

`just source-checks` and `just platform-checks`, both exit 0. Rollback: `git revert` the two
commits; nothing persists outside the repository.
