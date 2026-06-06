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
