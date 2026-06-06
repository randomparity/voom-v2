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
