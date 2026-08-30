#!/usr/bin/env bash

set -uo pipefail

failures=0

fail() {
	echo "FAIL: $1" >&2
	failures=$((failures + 1))
}

expect_field() {
	local label=$1 field=$2 want=$3
	shift 3
	local plan got
	plan=$(just "$@" 2>/dev/null)
	got=$(printf '%s\n' "$plan" | awk -F'\t' -v k="$field" '$1 == k {print $2}')
	[ "$got" = "$want" ] || fail "$label: expected $field=$want, got ${got:-<missing>}"
}

expect_field "test limits" load 1 test-constrained --load 1 --print-plan
expect_field "test command" command "just test" \
	test-constrained --load 1 --print-plan
expect_field "stress limits" write-bps 40M \
	stress-constrained --write-bps 40M --print-plan
expect_field "stress command" command "just stress" \
	stress-constrained --write-bps 40M --print-plan
expect_field "scale limits" memory 8G \
	scan-session-scale-constrained --memory 8G --print-plan
expect_field "scale command" command "just scan-session-scale" \
	scan-session-scale-constrained --memory 8G --print-plan

if [ "$failures" -gt 0 ]; then
	echo "constrained-recipes-selftest: $failures failure(s)" >&2
	exit 1
fi
echo "constrained-recipes-selftest: all checks passed"
