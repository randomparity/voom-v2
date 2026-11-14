# Bundled SQLite advisory deferral, quinn-udp and rustix bumps — design (#857, #858, #861)

## Problem

`just platform-checks` runs `cargo deny check` and `cargo audit --deny warnings`; the `audit`
workflow (`.github/workflows/audit.yml`) runs `rustsec/audit-check`. Three advisories against
`Cargo.lock` at `818cc91a` keep them red:

- **RUSTSEC-2026-0459** (#857): `libsqlite3-sys` 0.30.1 bundles SQLite < 3.54.0
  (CVE-2026-119375). Patched only in `libsqlite3-sys` >= 0.39. No released sqlx accepts it:
  `sqlx-sqlite` 0.8.6 requires `^0.30.1`, 0.9.0 requires `>=0.30.1, <0.38.0` (crates.io).
  `libsqlite3-sys` is a direct `bundled` dependency of `crates/voom-store/Cargo.toml` and must
  stay link-compatible with sqlx's, so it cannot move alone. Both tools flag it.
- **RUSTSEC-2026-0538** (#858): `quinn-udp` 0.5.15, fixed in 0.5.16 inside the 0.5 range.
  `cargo tree -i quinn-udp --target all -e normal` prints nothing (lockfile-only entry).
  Only `cargo audit` flags it; `cargo deny check advisories` does not.
- **RUSTSEC-2026-0486** (#861): `rustix` 1.1.4 is `unsound` (xattr functions), fixed in 1.1.6.
  It is in the normal graph (`cargo tree -i rustix -e normal --target all`: `tempfile` and
  `voom-ffprobe-worker`). Only `cargo audit --deny warnings` flags it.

No single advisory can be cleared green on its own, so one PR carries all three (operator
sequencing and widening decisions, 2026-11-14).

## Scope

Changed files, and nothing else:

| File | Change |
|---|---|
| `deny.toml` | `[advisories] ignore = [{ id = "RUSTSEC-2026-0459", reason = "<names docs/debt/0008>" }]` (cargo-deny 0.19 reasoned form) |
| `.cargo/audit.toml` | add `RUSTSEC-2026-0459` to `ignore`, with a comment block in the RUSTSEC-2023-0071 style pointing at the debt record |
| `docs/debt/0008-bundled-sqlite-advisory-awaits-an-sqlx-release.md` | new deferral record in the 0007 format |
| `Cargo.lock` | `cargo update -p quinn-udp --precise 0.5.16` and `cargo update -p rustix --precise 1.1.6`; dry runs move only those two packages |

**Commit order.** The prek `cargo-deny` hook runs on `deny.toml`/`Cargo.*` changes and
`cargo-audit` on `Cargo.lock` changes. Commit 1 carries the debt record and both ignores
(only `cargo-deny` runs; it then passes, since it flags only RUSTSEC-2026-0459). Commit 2
carries both lockfile bumps together: a commit bumping only one of them would leave
`cargo audit` red on the other.

The debt record carries: `Status` (`Open`, `review-by: 2027-02-14`); `Concern` (the advisory
and why no fix is reachable); `Exposure` (below); `Why deferred`; `Non-regression boundary`
(below); `What would resolve it` — an sqlx release whose `sqlx-sqlite` permits
`libsqlite3-sys >= 0.39`, then bump both and delete both ignores; `Provenance` (`target:` lines
and `tracker: #857`).

**Exposure** (bounded to current code paths at `818cc91a`): the only production opener of a
SQLite file is `voom_store::pool::connect` / `connect_or_create`
(`crates/voom-store/src/pool.rs`), which opens the operator-configured `database_url` (default
`<data_dir>/voom.db`, `crates/voom-core/src/runtime/config.rs` `default_database_url`) in WAL
mode. No production path imports, attaches, or opens a database file received from another
party. Exploitation needs a crafted `.db` + `-wal` pair at that operator-chosen path.

**Non-regression boundary** (what keeps the deferral honest until exit): the ignore names
RUSTSEC-2026-0459 only, so every other advisory still fails both tools; any new code path that
opens a SQLite file not created by this process from operator configuration (import, restore,
attach, upload) invalidates the exposure analysis and must reopen the record before landing.

Not changed: sqlx, `libsqlite3-sys` requirement or features, any `Cargo.toml`, any source code,
any workflow. No ADR: the remediation has no open alternative — the operator chose it and the
exclusions own the others.

**CI coverage.** `rustsec/audit-check` v2.0.0 (`src/main.ts` at the pinned SHA) runs
`cargo audit --json --file ./Cargo.lock` from the workspace root, so it reads
`.cargo/audit.toml`. Verified locally: from the repo root the existing RUSTSEC-2023-0071 ignore
is honored; from a foreign working directory it is not. The new ignore therefore also governs
the `audit` workflow; no workflow change is needed.

## Failure model

1. **Actors and deployments** — local operator running `voom` against a database path they
   configured; CI running `just platform-checks` and the `audit` workflow.
2. **Invariants and assets at stake** — the advisory gate must keep failing on any advisory
   other than RUSTSEC-2026-0459: the ignore names one ID; `severity_threshold`,
   `informational_warnings`, and `yanked = "deny"` are unchanged. RUSTSEC-2026-0538 and
   RUSTSEC-2026-0486 are fixed, never ignored.
3. **Accepted failure classes** — CVE-2026-119375 stays present in shipped binaries until the
   exit condition; accepted because the trigger needs an attacker-written `.db`/`-wal` pair at
   an operator-chosen path, which no current code path opens. A forgotten ignore is bounded by
   the record's `review-by` date.
4. **Covered elsewhere** — replacing sqlx with rusqlite (ADR 0003 owner); sqlx 0.9 bump
   (separate issue); system SQLite (deployment owner); stale review-by dates (`$warding`).

### Threat model

- **Boundary inventory** — none added or widened. Existing boundary: the SQLite file and its
  `-wal` sidecar in the operator's data directory.
- **Actor model** — trusted: the local operator. Untrusted: a party able to write a file the
  operator later points `database_url` at; no current code path imports such a file.
- **Control per boundary** — filesystem ownership of the operator's data directory; the
  deferral record, its review-by date, and its non-regression boundary govern the exception.
- **Explicitly out of scope** — shared or network filesystems where another user can write the
  data directory: outside the named deployment, and named in the record so its owner sees it.

## Success

1. At the branch head, `cargo deny check` exits 0 and `cargo audit --deny warnings` exits 0.
2. Removing the RUSTSEC-2026-0459 ignore from either policy file makes that tool fail again on
   that ID (the ignore is load-bearing).
3. `git diff origin/main -- Cargo.lock` changes only the `quinn-udp` and `rustix` `version` and
   `checksum` lines.
4. `cargo audit --json --file ./Cargo.lock` from the repo root (the workflow's invocation)
   reports `vulnerabilities.found = false`.
5. Each of the two implementation commits passes the prek hooks without bypass.
6. `just source-checks` and `just platform-checks` exit 0.

## Validation

- Criteria 1–2: `just deny`, `just audit`; then temporarily drop the ID from each file, rerun,
  observe failure naming RUSTSEC-2026-0459, restore.
- Criterion 3: lockfile diff inspection.
- Criterion 4: the JSON invocation piped to `jq .vulnerabilities.found`.
- Criterion 5: commit without `--no-verify`; `git status` clean after each.
- Criterion 6: the two guardrail recipes.
- The debt record prose: `task-test-not-applicable` — no executable consumer validates
  `docs/debt/` (no gate in `justfile`, `scripts/`, `.github/`, or `.pre-commit-config.yaml`).
