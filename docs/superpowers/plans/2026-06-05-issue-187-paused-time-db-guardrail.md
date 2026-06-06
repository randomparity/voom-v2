# Paused-time + SQLite-pool Test Guardrail Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an `ast-grep`-based CI check plus a written `AGENTS.md` convention that prevent any test from pairing tokio's paused virtual clock (`tokio::time::pause()`/`advance()`) with a real `SqlitePool`.

**Architecture:** A shell script `scripts/check-paused-time-db.sh` scans both sibling unit tests (`crates/*/src/**/*_test.rs`) and integration tests (`crates/*/tests/**/*.rs`); it flags a file that has **both** a paused-time signal and a DB-pool signal. A sibling self-test `scripts/check-paused-time-db-selftest.sh` exercises the check against throwaway fixtures so the `ast-grep` patterns cannot silently rot. Both are wired into the `just ci` target. The convention is recorded in the `AGENTS.md` "Testing layout" section.

**Tech Stack:** Bash (`set -euo pipefail`), `ast-grep` (`scan --inline-rules --json=stream`), `just`, `shellcheck`, `shfmt`.

**Spec:** `docs/superpowers/specs/2026-06-05-issue-187-paused-time-db-guardrail-design.md`
**ADR:** `docs/adr/0012-paused-time-db-pool-guard.md`

---

## Background the implementer needs

- **Why the trap exists.** Under `tokio::time::pause()` the runtime auto-advances virtual time to the next timer whenever it has no runnable task. `sqlx` runs SQLite on a blocking thread; while an `await` is parked on that thread the runtime is idle, so the paused clock jumps past the pool's `acquire_timeout` (sqlx default 30s) and the DB call fails with `DbUnreachable`. PR #186 fixed the one affected test; this guard stops reintroduction.
- **Verified `ast-grep` facts (v0.42.3) the script depends on:**
  - `ast-grep run --pattern 'SqlitePool'` compiles to an `identifier` pattern and does **not** match a *type* usage like `pool: &SqlitePool` (that's a `type_identifier` node). The pool signal therefore uses `ast-grep scan --inline-rules` with `any: [{kind: identifier, regex}, {kind: type_identifier, regex}]`, which matches both positions and — being anchored `^(SqlitePool|ControlPlane)$` — rejects `SqlitePoolOptions`/`ControlPlaneConfig`.
  - A bare `advance($$$)` pattern matches a free call `advance(..)` but **not** a method call `clock.advance(..)` / `fixture.clock.advance(..)` (a field-expression callee — different node), so the injected `ManualClock` is never flagged.
  - `ast-grep scan` with `severity: error` exits non-zero when a rule matches; `--json=stream` emits one JSON object per match containing `"file":"<path>"`. Paths in this repo never contain `"`, so `grep -oE '"file":"[^"]*"'` extracts them without `jq`.
  - `use tokio::time::{$$$};` (brace import) and `use tokio::time::$$$;` (single-path import) are **distinct** nodes; the import gate needs both patterns.

## File structure

- **Create** `scripts/check-paused-time-db.sh` — the guard. CWD-relative file discovery; one `ast-grep scan` per logical signal; co-occurrence logic; non-zero exit + actionable message on violation.
- **Create** `scripts/check-paused-time-db-selftest.sh` — the guard's own test. Lays out fixtures in `mktemp -d` trees and asserts the guard's exit code per case.
- **Modify** `justfile` — add `check-paused-time-db` and `check-paused-time-db-selftest` recipes and insert both into the `ci` target.
- **Modify** `AGENTS.md` — add the convention to the "Testing layout" section.

Two commits, each leaving `just ci` green:
1. the enforcement mechanism (both scripts + `justfile` wiring);
2. the written convention (`AGENTS.md`).

---

## Task 1: Add the check and its self-test, wired into CI

**Files:**
- Create: `scripts/check-paused-time-db.sh`
- Create: `scripts/check-paused-time-db-selftest.sh`
- Modify: `justfile` (recipes + `ci` target)

- [ ] **Step 1: Write the failing self-test**

Create `scripts/check-paused-time-db-selftest.sh` with exactly:

```bash
#!/usr/bin/env bash
# Self-test for check-paused-time-db.sh. Lays out fixture test files in a
# throwaway tree, runs the real check there, and asserts its exit code per case.
# Wired into `just ci` so the check's ast-grep patterns cannot silently rot.

set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
check="$script_dir/check-paused-time-db.sh"

failures=0

# Run the check inside a fresh fixture tree and assert its exit code.
#   expect_exit <expected-code> <relative-fixture-path> <file-contents>
expect_exit() {
	local want="$1" rel="$2" body="$3"
	local work
	work=$(mktemp -d)
	mkdir -p "$work/$(dirname "$rel")"
	printf '%s\n' "$body" >"$work/$rel"
	local got=0
	(cd "$work" && "$check" >/dev/null 2>&1) || got=$?
	rm -rf "$work"
	if [[ "$got" -ne "$want" ]]; then
		echo "FAIL: $rel — expected exit $want, got $got" >&2
		failures=$((failures + 1))
	fi
}

# --- Violations (exit 1): paused time + real pool in one file ---
expect_exit 1 crates/x/src/a_test.rs \
	'use super::*;
async fn t() { tokio::time::pause(); let _p: SqlitePool = todo!(); }'

expect_exit 1 crates/x/src/b_test.rs \
	'async fn t() { tokio::time::advance(Duration::from_secs(5)).await; let cp = ControlPlane::open(); }'

expect_exit 1 crates/x/src/c_test.rs \
	'use tokio::time::{pause, advance};
async fn t() { pause(); let _p: SqlitePool = todo!(); }'

expect_exit 1 crates/x/src/d_test.rs \
	'use tokio::time;
async fn t() { time::pause(); let cp = ControlPlane::open(); }'

expect_exit 1 crates/x/tests/integration.rs \
	'async fn t() { tokio::time::pause(); let _p: SqlitePool = todo!(); }'

# --- Clean (exit 0) ---
expect_exit 0 crates/x/src/e_test.rs \
	'async fn t() { tokio::time::pause(); tokio::time::advance(Duration::from_secs(5)).await; }'

expect_exit 0 crates/x/src/f_test.rs \
	'async fn t() { let _p: SqlitePool = todo!(); let cp = ControlPlane::open(); }'

expect_exit 0 crates/x/src/g_test.rs \
	'fn t() { clock.advance(Duration::seconds(60)); fixture.clock.advance(x); let cp = ControlPlane::open(); }'

expect_exit 0 crates/x/src/h_test.rs \
	'fn t() { let q: SqlitePoolOptions = w; let r: ControlPlaneConfig = z; tokio::time::pause(); }'

expect_exit 0 crates/x/src/i_test.rs \
	'async fn t() { // tokio::time::pause()
	let _p: SqlitePool = todo!(); }'

if [[ "$failures" -gt 0 ]]; then
	echo "check-paused-time-db-selftest: $failures case(s) failed." >&2
	exit 1
fi

echo "check-paused-time-db-selftest: OK"
```

- [ ] **Step 2: Run the self-test to verify it fails**

Run: `chmod +x scripts/check-paused-time-db-selftest.sh && ./scripts/check-paused-time-db-selftest.sh`
Expected: FAIL — every case errors because `scripts/check-paused-time-db.sh` does not exist yet (the `"$check"` invocation fails). The script exits non-zero.

- [ ] **Step 3: Write the check (minimal implementation)**

Create `scripts/check-paused-time-db.sh` with exactly:

```bash
#!/usr/bin/env bash
# Guard against pairing tokio's paused virtual clock with a real SqlitePool.
#
# A test that pairs tokio::time::pause()/advance() (the auto-advancing virtual
# clock) with a real SqlitePool fails spuriously: while an await is parked on
# sqlx's blocking SQLite thread the runtime is idle, so the paused clock jumps
# past the pool's acquire_timeout and the DB call errors with DbUnreachable.
#
# This check fails when a single test file contains BOTH a tokio paused-time
# call AND a DB-pool reference (SqlitePool or ControlPlane). It scans sibling
# unit tests (crates/*/src/**/*_test.rs) and integration tests
# (crates/*/tests/**/*.rs). The injected domain clock (ManualClock, called as
# the method clock.advance(...)) is a different syntax node and is never matched.
#
# Uses ast-grep (not ripgrep) so it operates on real Rust syntax-tree nodes and
# cannot be fooled by comments, string literals, or near-miss identifiers
# (SqlitePoolOptions, ControlPlaneConfig).
#
# See docs/adr/0012-paused-time-db-pool-guard.md and AGENTS.md (Testing layout).

set -euo pipefail

if ! command -v ast-grep >/dev/null; then
	echo "check-paused-time-db: ast-grep is required. Run 'just setup' to install." >&2
	exit 2
fi

mapfile -t test_files < <(find crates -type f \
	\( -path '*/src/*_test.rs' -o -path '*/tests/*.rs' \) 2>/dev/null | sort)

if [[ "${#test_files[@]}" -eq 0 ]]; then
	echo "check-paused-time-db: OK (no test files found)"
	exit 0
fi

# Pool reference: SqlitePool or ControlPlane as an exact identifier, in either
# expression position (identifier) or type position (type_identifier).
rule_pool='
id: pool-ref
language: rust
severity: error
rule:
  any:
    - { kind: identifier, regex: "^(SqlitePool|ControlPlane)$" }
    - { kind: type_identifier, regex: "^(SqlitePool|ControlPlane)$" }
'

# Paused-time call in fully-qualified or module-scoped form.
rule_paused_direct='
id: paused-direct
language: rust
severity: error
rule:
  any:
    - { pattern: "tokio::time::pause()" }
    - { pattern: "tokio::time::advance($$$)" }
    - { pattern: "time::pause()" }
    - { pattern: "time::advance($$$)" }
'

# Bare pause()/advance() free-function call. Method calls (clock.advance(...))
# are a field-expression callee and do not match.
rule_bare='
id: bare-call
language: rust
severity: error
rule:
  any:
    - { pattern: "pause()" }
    - { pattern: "advance($$$)" }
'

# A use-import that brings a tokio::time item into scope (gates the bare form).
rule_import='
id: tokio-time-import
language: rust
severity: error
rule:
  any:
    - { pattern: "use tokio::time::{$$$};" }
    - { pattern: "use tokio::time::$$$;" }
'

# Files (among test_files) in which an inline rule matches, one path per line.
# scan exits non-zero when an error-severity rule matches, so tolerate that
# under `set -e`. File paths in this repo never contain '"'.
matching_files() {
	local rule="$1"
	ast-grep scan --inline-rules "$rule" --json=stream "${test_files[@]}" 2>/dev/null |
		grep -oE '"file":"[^"]*"' | sed 's/^"file":"//; s/"$//' | sort -u || true
}

pool=$(matching_files "$rule_pool")
paused_direct=$(matching_files "$rule_paused_direct")
bare=$(matching_files "$rule_bare")
imports=$(matching_files "$rule_import")

# Bare calls count as paused only when the file also imports tokio::time.
bare_gated=""
if [[ -n "$bare" && -n "$imports" ]]; then
	bare_gated=$(comm -12 <(printf '%s\n' "$bare") <(printf '%s\n' "$imports") || true)
fi

paused=$(printf '%s\n%s\n' "$paused_direct" "$bare_gated" | sort -u | sed '/^$/d')

violations=""
if [[ -n "$paused" && -n "$pool" ]]; then
	violations=$(comm -12 <(printf '%s\n' "$paused") <(printf '%s\n' "$pool") || true)
fi

if [[ -n "$violations" ]]; then
	echo "check-paused-time-db: paused tokio time paired with a real SQLite pool:" >&2
	while IFS= read -r violation; do
		[[ -z "$violation" ]] && continue
		printf '  %s\n' "$violation" >&2
	done <<<"$violations"
	echo "Drive DB-touching tests on real time; control domain time via the injected" >&2
	echo "Clock (ManualClock). See AGENTS.md (Testing layout) and docs/adr/0012." >&2
	exit 1
fi

echo "check-paused-time-db: OK"
```

- [ ] **Step 4: Run the self-test to verify it passes**

Run: `chmod +x scripts/check-paused-time-db.sh && ./scripts/check-paused-time-db-selftest.sh`
Expected: `check-paused-time-db-selftest: OK` (exit 0).

- [ ] **Step 5: Run the check against the real tree (no false positives)**

Run: `./scripts/check-paused-time-db.sh`
Expected: `check-paused-time-db: OK` (exit 0). In particular `crates/voom-control-plane/src/scan/worker_test.rs` (which calls `tokio::time::pause()` but references no pool) is **not** flagged.

- [ ] **Step 6: Lint and format both scripts**

Run: `shellcheck scripts/check-paused-time-db.sh scripts/check-paused-time-db-selftest.sh && shfmt -d scripts/check-paused-time-db.sh scripts/check-paused-time-db-selftest.sh`
Expected: no output from `shellcheck`; no diff from `shfmt -d` (both are tab-indented, matching `scripts/check-test-layout.sh`). If `shfmt -d` prints a diff, run `shfmt -w scripts/check-paused-time-db.sh scripts/check-paused-time-db-selftest.sh` and re-run.

- [ ] **Step 7: Wire both into the justfile**

In `justfile`, change the `ci` target line from:

```
ci: fmt-check lint check-test-layout test doc deny audit
```

to:

```
ci: fmt-check lint check-test-layout check-paused-time-db check-paused-time-db-selftest test doc deny audit
```

Then add these two recipes immediately after the existing `check-test-layout` recipe:

```
# Guard: no test pairs tokio paused time with a real SqlitePool
check-paused-time-db:
    ./scripts/check-paused-time-db.sh

# Self-test for the paused-time guard (keeps its ast-grep patterns honest)
check-paused-time-db-selftest:
    ./scripts/check-paused-time-db-selftest.sh
```

- [ ] **Step 8: Verify the just recipes run**

Run: `just check-paused-time-db && just check-paused-time-db-selftest`
Expected: `check-paused-time-db: OK` then `check-paused-time-db-selftest: OK`.

- [ ] **Step 9: Commit**

```bash
git add scripts/check-paused-time-db.sh scripts/check-paused-time-db-selftest.sh justfile
git commit -m "ci: guard against paused tokio time with a real SQLite pool

Add scripts/check-paused-time-db.sh (ast-grep) flagging any test that
pairs tokio::time::pause()/advance() with a SqlitePool/ControlPlane
reference, plus a self-test, both wired into just ci.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: Document the convention in AGENTS.md

**Files:**
- Modify: `AGENTS.md` (the "Testing layout" section, after the `check-test-layout` paragraph)

- [ ] **Step 1: Add the convention paragraph**

In `AGENTS.md`, find the "Testing layout" section. After the paragraph that ends with `See docs/adr/0004-sibling-unit-tests.md.` and before the `just coverage` paragraph, insert:

```markdown
**Never pair `tokio::time::pause()`/`advance()` with a real `SqlitePool`.**
When tokio's clock is paused it auto-advances virtual time whenever the runtime
is idle — including while an `await` is parked on sqlx's blocking SQLite thread
— so the paused clock jumps past the pool's `acquire_timeout` and DB calls fail
spuriously with `DbUnreachable`. Drive DB-touching tests on real time and
control *domain* time through the injected `Clock` (`ManualClock`).
`just check-paused-time-db` (wired into `just ci`) enforces this: it fails when
one test file references `SqlitePool`/`ControlPlane` and also calls
`tokio::time::pause`/`advance`. See `docs/adr/0012-paused-time-db-pool-guard.md`.
```

- [ ] **Step 2: Verify the rule names both required terms**

Run: `grep -E 'tokio::time::pause' AGENTS.md && grep -E 'ManualClock' AGENTS.md`
Expected: both grep commands print a matching line (acceptance criterion 1).

- [ ] **Step 3: Commit**

```bash
git add AGENTS.md
git commit -m "docs(agents): forbid paused tokio time with a real SQLite pool

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: Full guardrail run

**Files:** none (verification only)

- [ ] **Step 1: Run the full CI suite**

Run: `just ci`
Expected: all steps pass, including `check-paused-time-db: OK` and `check-paused-time-db-selftest: OK`. Zero warnings.

- [ ] **Step 2: Confirm the working tree is clean**

Run: `git status --short`
Expected: no output.

---

## Self-Review

**1. Spec coverage:**
- Convention in `AGENTS.md` naming `tokio::time::pause` + `ManualClock` → Task 2 (acceptance 1).
- Check exists, wired into `just ci`, exits 0 on current tree, non-zero on violation fixtures → Task 1 steps 5, 7, and the self-test (acceptance 2).
- Does not flag `worker_test.rs` → Task 1 step 5 (acceptance 3).
- ADR 0012 with all sections, linked from spec, listed in `docs/adr/README.md` → already committed before this plan (acceptance 4).
- Self-test runs in `just ci`; scripts pass `shellcheck`/`shfmt`; `set -euo pipefail` → Task 1 steps 6–8 (acceptance 5).
- Scan covers both `crates/*/src/**/*_test.rs` and `crates/*/tests/**/*.rs` → check script `find` glob; self-test integration case (`crates/x/tests/integration.rs`).
- Detection: three paused-time call forms + import gate + exact-identifier pool match, excluding `ManualClock` method call → check script rules; self-test cases c, d, g, h.

**2. Placeholder scan:** No `TODO`/`TBD`/"similar to"/"add error handling" — every script and edit is shown in full.

**3. Type/name consistency:** Recipe names (`check-paused-time-db`, `check-paused-time-db-selftest`), script paths, and rule ids are identical across the `ci` target, the recipes, the self-test's `$check` path, and the `AGENTS.md` reference.
