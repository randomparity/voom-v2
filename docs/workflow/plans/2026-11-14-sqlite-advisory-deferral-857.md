# Bundled SQLite advisory deferral and quinn-udp bump — plan (#857, #858)

**Goal:** make `just platform-checks` green by deferring RUSTSEC-2026-0459 under a recorded,
time-boxed ignore and fixing RUSTSEC-2026-0538 with a lockfile bump.

**Architecture:** configuration and documentation only. Two advisory-policy files gain one
narrow ignore each, a `docs/debt/` record owns the exception, and `Cargo.lock` moves one crate.

**Tech stack:** cargo 1.95.0, cargo-deny 0.19.0, cargo-audit 0.22.2, just.

Spec: `docs/workflow/specs/2026-11-14-sqlite-advisory-deferral-857-design.md`.

Expected implementation size: 90–130 changed lines (M) — the debt record (~70 lines), two
policy-file edits (~15 lines), and the quinn-udp lockfile entry (~4 lines).

## Global Constraints

- Ignore exactly `RUSTSEC-2026-0459`. Never ignore `RUSTSEC-2026-0538`.
- Do not change sqlx, `libsqlite3-sys`, any `Cargo.toml`, or any source file.
- `Cargo.lock` changes only `quinn-udp` 0.5.15 -> 0.5.16 (resolved on crates.io, not yanked).
- Debt record number is `0008` (assigned); review-by `2027-02-14`.
- Lines <= 100 chars; conventional commits; run hooks with a raised timeout and check
  `git status` after each commit (prek can drop unstaged files on timeout).

## File map

| File | Owns today | Change |
|---|---|---|
| `Cargo.lock` | resolved graph | quinn-udp 0.5.16 |
| `docs/debt/0008-bundled-sqlite-advisory-awaits-an-sqlx-release.md` | new | the deferral |
| `deny.toml` | cargo-deny policy | one reasoned ignore |
| `.cargo/audit.toml` | cargo-audit policy | one ignore + comment |

## Task 1 — Fix RUSTSEC-2026-0538 by bumping quinn-udp

**Interfaces:** none consumed; Task 2 relies on a lockfile free of RUSTSEC-2026-0538.

**Verification:**
- Contract: no advisory for quinn-udp. `Mode: focused-test` — observable: `cargo audit
  --deny warnings` output. Red before: output lists `RUSTSEC-2026-0538`. Green: `cargo audit
  --deny warnings 2>&1 | rg -c RUSTSEC-2026-0538` prints nothing (exit 1 from rg).
- Contract: lockfile scope. `Mode: focused-test` — `git diff -- Cargo.lock` shows only the
  quinn-udp `version` and `checksum` lines.

Steps:
1. `cargo audit --deny warnings 2>&1 | rg RUSTSEC-2026-0538` — expect a match (red).
2. `cargo update -p quinn-udp --precise 0.5.16` — expect `Updating quinn-udp v0.5.15 -> v0.5.16`.
3. `git diff --stat -- Cargo.lock` — expect `1 file changed, 2 insertions(+), 2 deletions(-)`.
4. Rerun step 1 — expect no match.
5. Commit: `fix(deps): bump quinn-udp to 0.5.16 for RUSTSEC-2026-0538`.

## Task 2 — Defer RUSTSEC-2026-0459 behind a recorded ignore

**Interfaces:** consumes Task 1's lockfile. Produces the debt record path that both ignores name.

**Verification:**
- Contract: deny ignore is load-bearing. `Mode: focused-test` — red: `cargo deny check
  advisories` fails naming `RUSTSEC-2026-0459`; green: `just deny` exits 0.
- Contract: audit ignore is load-bearing. `Mode: focused-test` — red: `cargo audit --deny
  warnings` fails naming `RUSTSEC-2026-0459`; green: `just audit` exits 0.
- Contract: debt record content. `Mode: task-test-not-applicable` — no executable consumer
  validates `docs/debt/` (`rg -n debt justfile scripts/` finds none); reviewed in step 6.

Steps:
1. Run `cargo deny check advisories` and `cargo audit --deny warnings`; expect both to fail on
   RUSTSEC-2026-0459 only (red).
2. Write `docs/debt/0008-bundled-sqlite-advisory-awaits-an-sqlx-release.md` with sections
   `Status` (`Open`, `review-by: 2027-02-14`), `Concern`, `Exposure`, `Why deferred`,
   `What would resolve it`, `Provenance` — content per the spec's Scope section, citing
   `crates/voom-store/Cargo.toml`, `crates/voom-store/src/pool.rs`,
   `crates/voom-core/src/runtime/config.rs`, the crates.io requirements of `sqlx-sqlite`
   0.8.6 (`^0.30.1`) and 0.9.0 (`>=0.30.1, <0.38.0`), and `tracker: #857`.
3. In `deny.toml` replace `ignore = []` with:

   ```toml
   ignore = [
       # Bundled SQLite CVE-2026-119375; no released sqlx permits the fixed
       # libsqlite3-sys >= 0.39. Remove with the bump; see docs/debt/0008.
       { id = "RUSTSEC-2026-0459", reason = "awaiting sqlx release; docs/debt/0008" },
   ]
   ```

4. In `.cargo/audit.toml` add a `# RUSTSEC-2026-0459 — ...` comment block after the
   RUSTSEC-2023-0071 block (cause, why not fixable, exit condition, record path) and set
   `ignore = ["RUSTSEC-2023-0071", "RUSTSEC-2026-0459"]`.
5. `just deny` and `just audit` — expect exit 0. Then remove the ID from each file in turn,
   rerun the matching recipe, expect failure naming RUSTSEC-2026-0459, and restore.
6. Read the record against `docs/debt/0007-*.md` for section shape.
7. Commit: `fix(deps): defer RUSTSEC-2026-0459 until sqlx permits the fix`.

## Final guardrails

`just source-checks` and `just platform-checks`, both exit 0. Rollback: `git revert` the two
commits; nothing persists outside the repository.
