# Bundled SQLite advisory deferral and quinn-udp bump — design (#857, #858)

## Problem

`just platform-checks` runs `cargo deny check` and `cargo audit --deny warnings`. Both fail on
two advisories against `Cargo.lock` at `cbbdb185`:

- **RUSTSEC-2026-0459** (#857): `libsqlite3-sys` 0.30.1 bundles SQLite < 3.54.0
  (CVE-2026-119375). Patched only in `libsqlite3-sys` >= 0.39. No released sqlx accepts it:
  `sqlx-sqlite` 0.8.6 requires `^0.30.1`, 0.9.0 requires `>=0.30.1, <0.38.0` (crates.io).
  `libsqlite3-sys` is a direct `bundled` dependency of `crates/voom-store/Cargo.toml` and must
  stay link-compatible with sqlx's, so it cannot move alone.
- **RUSTSEC-2026-0538** (#858): `quinn-udp` 0.5.15. Fixed in 0.5.16, inside the 0.5 semver
  range. `cargo tree -i quinn-udp --target all -e normal` prints nothing: it is a lockfile-only
  entry, but both tools still flag it.

Neither can go green alone, so one PR carries both (operator sequencing decision).

## Scope

Changed files, and nothing else:

| File | Change |
|---|---|
| `deny.toml` | `[advisories] ignore = [{ id = "RUSTSEC-2026-0459", reason = "<names docs/debt/0008>" }]` (cargo-deny 0.19 reasoned form) |
| `.cargo/audit.toml` | add `RUSTSEC-2026-0459` to `ignore`, with a comment block in the RUSTSEC-2023-0071 style pointing at the debt record |
| `docs/debt/0008-bundled-sqlite-advisory-awaits-an-sqlx-release.md` | new deferral record in the 0007 format |
| `Cargo.lock` | `cargo update -p quinn-udp --precise 0.5.16`; the dry run shows only quinn-udp moves |

The debt record carries: Status `Open` with `review-by: 2027-02-14`; Concern (the advisory and
why no fix is reachable); Exposure (below); Why deferred; What would resolve it — an sqlx
release whose `sqlx-sqlite` permits `libsqlite3-sys >= 0.39`, then bump both and delete both
ignores; Provenance (`target:` lines and `tracker: #857`).

Exposure analysis for the record: the database URL is operator-configured (`database_url`,
default `<data_dir>/voom.db`, `crates/voom-core/src/runtime/config.rs`), opened by
`crates/voom-store/src/pool.rs` in WAL mode. Exploitation needs the process to open a crafted
`.db` + `-wal` pair written by a party less trusted than the operator. VOOM never opens a
database file it received from another party.

Not changed: sqlx, `libsqlite3-sys` requirement or features, any source code. No ADR: the
remediation has no open alternative — the operator chose it, and the exclusions own the others.

## Failure model

1. **Actors and deployments** — local operator running `voom` against a database path they
   configured; CI running `just platform-checks`.
2. **Invariants and assets at stake** — the advisory gate must keep failing on any *other*
   advisory: the ignore names one ID only; `severity_threshold`, `yanked = "deny"` unchanged.
   RUSTSEC-2026-0538 is fixed, never ignored.
3. **Accepted failure classes** — CVE-2026-119375 stays present in shipped binaries until the
   exit condition; accepted because the trigger needs an attacker-written `.db`/`-wal` pair at
   an operator-chosen path, outside the named deployment. A silently-forgotten ignore is
   bounded by the record's `review-by` date.
4. **Covered elsewhere** — replacing sqlx with rusqlite (ADR 0003 owner); sqlx 0.9 bump
   (separate issue); system SQLite (deployment owner); stale review-by dates (`$warding`).

### Threat model

- **Boundary inventory** — none added or widened. Existing boundary: the SQLite file and its
  `-wal` sidecar, controlled by the operator.
- **Actor model** — trusted: the local operator. Untrusted: a party able to write a file the
  operator later points `database_url` at; no current code path imports such a file.
- **Control per boundary** — filesystem ownership of the operator's data directory; the
  deferral record and its review-by date govern the advisory exception.
- **Explicitly out of scope** — shared or network filesystems where another user can write the
  data directory: outside the named deployment, and named in the record so its owner sees it.

## Success

1. `cargo deny check` exits 0 and `cargo audit --deny warnings` exits 0 at the branch head.
2. Removing the RUSTSEC-2026-0459 ignore from either file makes that tool fail again on that ID
   (the ignore is load-bearing and narrow).
3. `git diff origin/main -- Cargo.lock` changes only the `quinn-udp` version and checksum.
4. `just source-checks` and `just platform-checks` exit 0.

## Validation

- Criteria 1–2: `just deny`, `just audit`; then temporarily drop the ID from each file, rerun,
  observe failure naming RUSTSEC-2026-0459, restore.
- Criterion 3: lockfile diff inspection.
- Criterion 4: the two guardrail recipes.
- The debt record prose: `task-test-not-applicable` — no executable consumer validates
  `docs/debt/` (no gate in `justfile` or `scripts/`).
