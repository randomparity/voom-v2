#!/usr/bin/env bash
# Self-test for the control-plane SQL boundary guard. The fixture tree exercises
# every forbidden call shape plus the production/test path boundary.

set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
check="$script_dir/check-control-plane-sql-boundary.sh"
work=$(mktemp -d -t voom-control-plane-sql-boundary.XXXXXX)
trap 'rm -R "$work"' EXIT

failures=0

fail() {
	echo "FAIL: $1" >&2
	failures=$((failures + 1))
}

run_guard() {
	local root="$1"
	local output_var="$2"
	local status_var="$3"
	local guard_output
	local guard_status=0
	guard_output=$("$check" "$root" 2>&1) || guard_status=$?
	printf -v "$output_var" '%s' "$guard_output"
	printf -v "$status_var" '%s' "$guard_status"
}

mkdir -p "$work/violations"
cat >"$work/violations/forbidden.rs" <<'RUST'
use sqlx::query as db_query;

fn forbidden() {
    let _ = sqlx::query("SELECT 1");
    let _ = sqlx::query_as::<_, Row>("SELECT 1");
    let _ = sqlx::query_scalar::<_, i64>("SELECT 1");
    let _ = sqlx::query!(
        "SELECT 1"
    );
    let _ = sqlx::raw_sql("SELECT 1");
    let _ = sqlx::QueryBuilder::new("SELECT 1");
    let _ = sqlx::QueryBuilder::<Sqlite>::new(
        "SELECT 1"
    );
    let _ = db_query(
        "SELECT 1"
    );
}
RUST

violation_output=""
violation_status=0
run_guard "$work/violations" violation_output violation_status
if [[ "$violation_status" -ne 1 ]]; then
	fail "violation fixture expected exit 1, got $violation_status: $violation_output"
fi

for expected in \
	'sqlx::query' \
	'sqlx::query_as' \
	'sqlx::query_scalar' \
	'sqlx::query!' \
	'sqlx::raw_sql' \
	'sqlx::QueryBuilder::new'; do
	if ! grep -Fq "$expected" <<<"$violation_output"; then
		fail "diagnostics missing forbidden API $expected"
	fi
done

diagnostic_pattern='forbidden\.rs:[0-9]+.*Move SQL into a typed voom-store repository method'
if ! grep -Eq "$diagnostic_pattern" <<<"$violation_output"; then
	fail "diagnostics must contain file, line, and the repository-boundary fix"
fi

diagnostic_count=$(grep -Ec "$diagnostic_pattern" <<<"$violation_output" || true)
if [[ "$diagnostic_count" -ne 8 ]]; then
	fail "expected all 8 violations in one run, got $diagnostic_count"
fi

mkdir -p "$work/clean/tests"
cat >"$work/clean/clean.rs" <<'RUST'
fn clean() {
    let _ = query("not sqlx");
    let _ = query_as("not sqlx");
    let _ = sqlx_query("not sqlx");
    let _ = other::QueryBuilder::new("not sqlx");
}
RUST
cat >"$work/clean/fixture_test.rs" <<'RUST'
fn test_fixture() {
    let _ = sqlx::query("allowed in sibling tests");
}
RUST
cat >"$work/clean/tests/integration.rs" <<'RUST'
fn integration_fixture() {
    let _ = sqlx::raw_sql("allowed in integration tests");
}
RUST

clean_output=""
clean_status=0
run_guard "$work/clean" clean_output clean_status
if [[ "$clean_status" -ne 0 ]]; then
	fail "clean fixture expected exit 0, got $clean_status: $clean_output"
fi
if [[ "$clean_output" != "control-plane SQL boundary: OK" ]]; then
	fail "clean fixture emitted unexpected success output: $clean_output"
fi

if [[ "$failures" -gt 0 ]]; then
	echo "check-control-plane-sql-boundary-selftest: $failures failure(s)." >&2
	exit 1
fi

echo "check-control-plane-sql-boundary-selftest: OK"
