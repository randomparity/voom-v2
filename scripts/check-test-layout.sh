#!/usr/bin/env bash
# Enforce the sibling-test layout convention.
#
# Fails if:
#   1. Any file under crates/*/src/ contains an active
#      `#[cfg(test)] mod $NAME { ... }` inline module item.
#   2. Any crates/*/src/**/*_test.rs lacks an active sibling
#      `#[cfg(test)] #[path = "<basename>_test.rs"] mod tests;`
#      declaration in its parent source file.
#
# Uses ast-grep (not ripgrep) so the check operates on real Rust
# syntax tree items and cannot be fooled by comments, string
# literals, or items behind disabled cfgs.
#
# See docs/superpowers/specs/2026-05-16-sibling-tests-and-sonarcloud-design.md
# section 3 for design.

set -euo pipefail

if ! command -v ast-grep >/dev/null; then
	echo "check-test-layout: ast-grep is required. Run 'just setup' to install." >&2
	exit 2
fi

errors=0

# --- Check 1: no inline `#[cfg(test)] mod $NAME { ... }` items in src/ ---
# The pattern spans an attribute + mod_item, so we use --selector mod_item to
# anchor on the module declaration (ast-grep requires single-node patterns).
# shellcheck disable=SC2016  # $NAME and $$$ are ast-grep meta-variables, not shell vars
inline_hits=$(ast-grep run \
	--lang rust \
	--pattern '#[cfg(test)] mod $NAME { $$$ }' \
	--selector mod_item \
	crates/*/src 2>/dev/null || true)

if [[ -n "$inline_hits" ]]; then
	echo "check-test-layout: inline tests detected in src/:" >&2
	echo "$inline_hits" >&2
	echo "Move them to a sibling <source>_test.rs file. See docs/adr/0004-sibling-unit-tests.md." >&2
	errors=$((errors + 1))
fi

# --- Check 2: every *_test.rs has a matching #[path] declaration in its sibling ---
while IFS= read -r test_file; do
	[[ -z "$test_file" ]] && continue
	dir=$(dirname "$test_file")
	test_base=$(basename "$test_file") # e.g. version_test.rs
	src_base=${test_base%_test.rs}.rs  # e.g. version.rs
	src_file="$dir/$src_base"

	if [[ ! -f "$src_file" ]]; then
		echo "check-test-layout: $test_file has no sibling source file $src_file" >&2
		errors=$((errors + 1))
		continue
	fi

	# Look for an active #[cfg(test)] #[path = "<test_base>"] mod tests; item.
	# shellcheck disable=SC2016  # $MOD is an ast-grep meta-variable, not a shell var
	match=$(ast-grep run \
		--lang rust \
		--pattern '#[cfg(test)] #[path = "'"$test_base"'"] mod $MOD;' \
		--selector mod_item \
		"$src_file" 2>/dev/null || true)

	if [[ -z "$match" ]]; then
		echo "check-test-layout: $src_file is missing" >&2
		echo "    #[cfg(test)] #[path = \"$test_base\"] mod tests;" >&2
		echo "  Without it, $test_file is silently skipped by cargo test." >&2
		errors=$((errors + 1))
	fi
done < <(find crates -path '*/src/*_test.rs' -type f 2>/dev/null)

if [[ "$errors" -gt 0 ]]; then
	echo "check-test-layout: $errors violation(s)." >&2
	exit 1
fi

echo "check-test-layout: OK"
