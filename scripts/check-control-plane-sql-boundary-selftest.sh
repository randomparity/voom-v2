#!/usr/bin/env bash
# Self-test for the control-plane SQL boundary guard. Each forbidden syntax
# family lives in an isolated production fixture, then all run together to
# prove exhaustive diagnostics. Clean fixtures verify path and scope boundaries.

set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
check="$script_dir/check-control-plane-sql-boundary.sh"
work=$(mktemp -d -t voom-control-plane-sql-boundary.XXXXXX)
trap 'rm -R "$work"' EXIT

failures=0
total_expected=0
diagnostic_pattern='\.rs:[0-9]+ — forbidden .*Move SQL into a typed voom-store repository method'

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

expect_violations() {
	local label="$1"
	local root="$2"
	local expected_count="$3"
	shift 3
	local output=""
	local status=0
	run_guard "$root" output status
	if [[ "$status" -ne 1 ]]; then
		fail "$label expected exit 1, got $status: $output"
	fi
	local actual_count
	actual_count=$(grep -Ec "$diagnostic_pattern" <<<"$output" || true)
	if [[ "$actual_count" -ne "$expected_count" ]]; then
		fail "$label expected $expected_count diagnostics, got $actual_count"
	fi
	local api
	for api in "$@"; do
		if ! grep -Fq "forbidden $api." <<<"$output"; then
			fail "$label diagnostics missing forbidden API $api"
		fi
	done
	total_expected=$((total_expected + expected_count))
}

expect_clean() {
	local label="$1"
	local root="$2"
	local output=""
	local status=0
	run_guard "$root" output status
	if [[ "$status" -ne 0 ]]; then
		fail "$label expected exit 0, got $status: $output"
	fi
	if [[ "$output" != "control-plane SQL boundary: OK" ]]; then
		fail "$label emitted unexpected success output: $output"
	fi
}

violations="$work/violations"

mkdir -p "$violations/functions"
cat >"$violations/functions/forbidden.rs" <<'RUST'
fn functions() {
    let _ = sqlx::query("SELECT 1");
    let _ = sqlx::query_with::<Sqlite, _>("SELECT 1", args());
    let _ = sqlx::query_as::<_, Row>(
        "SELECT 1"
    );
    let _ = sqlx::query_as_with::<Sqlite, Row, _>("SELECT 1", args());
    let _ = sqlx::query_scalar::<_, i64>("SELECT 1");
    let _ = sqlx::query_scalar_with::<Sqlite, i64, _>("SELECT 1", args());
    let _ = sqlx::raw_sql("SELECT 1");
}
RUST
expect_violations functions "$violations/functions" 7 \
	'sqlx::query' 'sqlx::query_with' 'sqlx::query_as' 'sqlx::query_as_with' \
	'sqlx::query_scalar' 'sqlx::query_scalar_with' 'sqlx::raw_sql'

mkdir -p "$violations/macros"
cat >"$violations/macros/forbidden.rs" <<'RUST'
fn macros() {
    let _ = sqlx::query!("SELECT 1");
    let _ = sqlx::query_unchecked! { "SELECT 1" };
    let _ = sqlx::query_as![Row, "SELECT 1"];
    let _ = sqlx::query_as_unchecked!(Row, "SELECT 1");
    let _ = sqlx::query_scalar! { "SELECT 1" };
    let _ = sqlx::query_scalar_unchecked!["SELECT 1"];
    let _ = sqlx::query_file!("query.sql");
    let _ = sqlx::query_file_unchecked! { "query.sql" };
    let _ = sqlx::query_file_as![Row, "query.sql"];
    let _ = sqlx::query_file_as_unchecked!(Row, "query.sql");
    let _ = sqlx::query_file_scalar! { "query.sql" };
    let _ = sqlx::query_file_scalar_unchecked!["query.sql"];
}
RUST
expect_violations macros "$violations/macros" 12 \
	'sqlx::query!' 'sqlx::query_unchecked!' \
	'sqlx::query_as!' 'sqlx::query_as_unchecked!' \
	'sqlx::query_scalar!' 'sqlx::query_scalar_unchecked!' \
	'sqlx::query_file!' 'sqlx::query_file_unchecked!' \
	'sqlx::query_file_as!' 'sqlx::query_file_as_unchecked!' \
	'sqlx::query_file_scalar!' 'sqlx::query_file_scalar_unchecked!'

mkdir -p "$violations/builders"
cat >"$violations/builders/forbidden.rs" <<'RUST'
fn builders() {
    let _ = sqlx::QueryBuilder::new("SELECT 1");
    let _ = sqlx::QueryBuilder::<Sqlite>::new("SELECT 1");
    let _ = sqlx::QueryBuilder::with_arguments("SELECT 1", args());
    let _ = sqlx::QueryBuilder::<Sqlite>::with_arguments(
        "SELECT 1",
        args(),
    );
}
RUST
expect_violations builders "$violations/builders" 4 \
	'sqlx::QueryBuilder::new' 'sqlx::QueryBuilder::with_arguments'

mkdir -p "$violations/root_paths"
cat >"$violations/root_paths/forbidden.rs" <<'RUST'
fn root_paths() {
    let _ = ::sqlx::query("SELECT 1");
    let _ = ::sqlx::query_scalar_with::<Sqlite, i64, _>("SELECT 1", args());
    let _ = ::sqlx::query_as_unchecked! { Row, "SELECT 1" };
    let _ = ::sqlx::QueryBuilder::new("SELECT 1");
    let _ = ::sqlx::QueryBuilder::<Sqlite>::with_arguments("SELECT 1", args());
}
RUST
expect_violations root_paths "$violations/root_paths" 5 \
	'sqlx::query' 'sqlx::query_scalar_with' 'sqlx::query_as_unchecked!' \
	'sqlx::QueryBuilder::new' 'sqlx::QueryBuilder::with_arguments'

mkdir -p "$violations/imports"
cat >"$violations/imports/simple.rs" <<'RUST'
use sqlx::query;

fn simple_import() {
    let _ = query("SELECT 1");
}
RUST
cat >"$violations/imports/grouped.rs" <<'RUST'
use sqlx::{query_as, query_scalar_unchecked, QueryBuilder};

fn grouped_imports() {
    let _ = query_as::<_, Row>("SELECT 1");
    let _ = query_scalar_unchecked! { "SELECT 1" };
    let _ = QueryBuilder::<Sqlite>::new("SELECT 1");
}
RUST
expect_violations imports "$violations/imports" 4 \
	'sqlx::query' 'sqlx::query_as' 'sqlx::query_scalar_unchecked!' \
	'sqlx::QueryBuilder::new'

mkdir -p "$violations/item_aliases"
cat >"$violations/item_aliases/simple.rs" <<'RUST'
use sqlx::query as db_query;

fn simple_alias() {
    let _ = db_query("SELECT 1");
}
RUST
cat >"$violations/item_aliases/grouped.rs" <<'RUST'
use sqlx::{
    query_as_with as load_with,
    query_file_scalar_unchecked as scalar_file,
    query_unchecked as unchecked,
    QueryBuilder as DbBuilder,
};

fn grouped_aliases() {
    let _ = load_with::<Sqlite, Row, _>("SELECT 1", args());
    let _ = unchecked! { "SELECT 1" };
    let _ = scalar_file!["query.sql"];
    let _ = DbBuilder::<Sqlite>::with_arguments("SELECT 1", args());
}
RUST
expect_violations item_aliases "$violations/item_aliases" 5 \
	'sqlx::query' 'sqlx::query_as_with' 'sqlx::query_unchecked!' \
	'sqlx::query_file_scalar_unchecked!' 'sqlx::QueryBuilder::with_arguments'

mkdir -p "$violations/crate_aliases"
cat >"$violations/crate_aliases/simple.rs" <<'RUST'
use sqlx as db;

fn crate_alias() {
    let _ = db::query_with::<Sqlite, _>("SELECT 1", args());
    let _ = db::query_unchecked! { "SELECT 1" };
    let _ = db::QueryBuilder::<Sqlite>::with_arguments("SELECT 1", args());
}
RUST
cat >"$violations/crate_aliases/grouped.rs" <<'RUST'
use sqlx::{self as grouped_db};

fn grouped_crate_alias() {
    let _ = grouped_db::query_scalar("SELECT 1");
    let _ = grouped_db::query_as_unchecked![Row, "SELECT 1"];
    let _ = grouped_db::QueryBuilder::new("SELECT 1");
}
RUST
cat >"$violations/crate_aliases/extern_crate.rs" <<'RUST'
extern crate sqlx as legacy_db;

fn extern_crate_alias() {
    let _ = legacy_db::query_as_with::<Sqlite, Row, _>("SELECT 1", args());
    let _ = legacy_db::query_file_unchecked!("query.sql");
    let _ = legacy_db::QueryBuilder::<Sqlite>::new("SELECT 1");
}
RUST
expect_violations crate_aliases "$violations/crate_aliases" 9 \
	'sqlx::query_with' 'sqlx::query_unchecked!' \
	'sqlx::QueryBuilder::with_arguments' 'sqlx::query_scalar' \
	'sqlx::query_as_unchecked!' 'sqlx::QueryBuilder::new' \
	'sqlx::query_as_with' 'sqlx::query_file_unchecked!'

mkdir -p "$violations/visibility"
cat >"$violations/visibility/pub_crate_item.rs" <<'RUST'
pub(crate) use sqlx::query as db_query;

fn pub_crate_item() {
    let _ = db_query("SELECT 1");
}
RUST
cat >"$violations/visibility/pub_item.rs" <<'RUST'
pub use sqlx::query_as;

fn pub_item() {
    let _ = query_as::<_, Row>("SELECT 1");
}
RUST
cat >"$violations/visibility/restricted.rs" <<'RUST'
mod nested {
    pub(super) use sqlx::query_scalar;
    pub(self) use sqlx::query_with as query_with_args;
    pub(in crate) use sqlx::query_scalar_with;

    fn restricted() {
        let _ = query_scalar::<_, i64>("SELECT 1");
        let _ = query_with_args::<Sqlite, _>("SELECT 1", args());
        let _ = query_scalar_with::<Sqlite, i64, _>("SELECT 1", args());
    }
}
RUST
cat >"$violations/visibility/pub_crate_alias.rs" <<'RUST'
pub(crate) use sqlx as db;

fn pub_crate_alias() {
    let _ = db::query("SELECT 1");
}
RUST
cat >"$violations/visibility/pub_extern_crate.rs" <<'RUST'
pub extern crate sqlx as legacy_db;

fn pub_extern_crate() {
    let _ = legacy_db::query_as::<_, Row>("SELECT 1");
}
RUST
expect_violations visibility "$violations/visibility" 7 \
	'sqlx::query' 'sqlx::query_as' 'sqlx::query_scalar' \
	'sqlx::query_with' 'sqlx::query_scalar_with'

mkdir -p "$violations/wildcards"
cat >"$violations/wildcards/crate.rs" <<'RUST'
use sqlx::*;

fn crate_wildcard() {
    let _ = query("SELECT 1");
    let _ = query_unchecked! { "SELECT 1" };
    let _ = QueryBuilder::<Sqlite>::new("SELECT 1");
}
RUST
cat >"$violations/wildcards/query_builder.rs" <<'RUST'
use sqlx::query_builder::*;

fn query_builder_wildcard() {
    let _ = QueryBuilder::<Sqlite>::new("SELECT 1");
    let _ = QueryBuilder::<Sqlite>::with_arguments("SELECT 1", args());
}
RUST
expect_violations wildcards "$violations/wildcards" 2 \
	'sqlx::*' 'sqlx::query_builder::*'

mkdir -p "$violations/raw_aliases"
cat >"$violations/raw_aliases/item.rs" <<'RUST'
use sqlx::query as r#type;

fn raw_item_alias() {
    let _ = r#type("SELECT 1");
}
RUST
cat >"$violations/raw_aliases/crate.rs" <<'RUST'
use sqlx as r#match;

fn raw_crate_alias() {
    let _ = r#match::query("SELECT 1");
    let _ = r#match::query_unchecked! { "SELECT 1" };
    let _ = r#match::query_builder::QueryBuilder::<Sqlite>::new("SELECT 1");
}
RUST
cat >"$violations/raw_aliases/module.rs" <<'RUST'
use sqlx::query_builder as r#type;

fn raw_module_alias() {
    let _ = r#type::QueryBuilder::<Sqlite>::new("SELECT 1");
    let _ = r#type::QueryBuilder::<Sqlite>::with_arguments("SELECT 1", args());
}
RUST
expect_violations raw_aliases "$violations/raw_aliases" 6 \
	'sqlx::query' 'sqlx::query_unchecked!' \
	'sqlx::QueryBuilder::new' 'sqlx::QueryBuilder::with_arguments'

mkdir -p "$violations/query_builder_paths"
cat >"$violations/query_builder_paths/direct.rs" <<'RUST'
fn direct_module_path() {
    let _ = sqlx::query_builder::QueryBuilder::new("SELECT 1");
    let _ = sqlx::query_builder::QueryBuilder::<Sqlite>::with_arguments(
        "SELECT 1",
        args(),
    );
    let _ = ::sqlx::query_builder::QueryBuilder::with_arguments("SELECT 1", args());
    let _ = ::sqlx::query_builder::QueryBuilder::<Sqlite>::new("SELECT 1");
}
RUST
cat >"$violations/query_builder_paths/crate_alias.rs" <<'RUST'
use sqlx as db;

fn crate_alias_module_path() {
    let _ = db::query_builder::QueryBuilder::new("SELECT 1");
    let _ = db::query_builder::QueryBuilder::<Sqlite>::with_arguments("SELECT 1", args());
}
RUST
cat >"$violations/query_builder_paths/module_import.rs" <<'RUST'
use sqlx::query_builder;

fn module_import() {
    let _ = query_builder::QueryBuilder::new("SELECT 1");
    let _ = query_builder::QueryBuilder::<Sqlite>::with_arguments("SELECT 1", args());
}
RUST
cat >"$violations/query_builder_paths/grouped_module_import.rs" <<'RUST'
use sqlx::{query_builder};

fn grouped_module_import() {
    let _ = query_builder::QueryBuilder::new("SELECT 1");
    let _ = query_builder::QueryBuilder::<Sqlite>::with_arguments("SELECT 1", args());
}
RUST
cat >"$violations/query_builder_paths/module_alias.rs" <<'RUST'
use sqlx::query_builder as qb;

fn module_alias() {
    let _ = qb::QueryBuilder::new("SELECT 1");
    let _ = qb::QueryBuilder::<Sqlite>::with_arguments("SELECT 1", args());
}
RUST
cat >"$violations/query_builder_paths/grouped_module_alias.rs" <<'RUST'
use sqlx::{query_builder as qb};

fn grouped_module_alias() {
    let _ = qb::QueryBuilder::new("SELECT 1");
    let _ = qb::QueryBuilder::<Sqlite>::with_arguments("SELECT 1", args());
}
RUST
cat >"$violations/query_builder_paths/item_import.rs" <<'RUST'
use sqlx::QueryBuilder;

fn item_import() {
    let _ = QueryBuilder::new("SELECT 1");
    let _ = QueryBuilder::<Sqlite>::with_arguments("SELECT 1", args());
}
RUST
cat >"$violations/query_builder_paths/item_alias.rs" <<'RUST'
use sqlx::QueryBuilder as DbBuilder;

fn item_alias() {
    let _ = DbBuilder::new("SELECT 1");
    let _ = DbBuilder::<Sqlite>::with_arguments("SELECT 1", args());
}
RUST
expect_violations query_builder_paths "$violations/query_builder_paths" 18 \
	'sqlx::QueryBuilder::new' 'sqlx::QueryBuilder::with_arguments'

aggregate_output=""
aggregate_status=0
run_guard "$violations" aggregate_output aggregate_status
aggregate_count=$(grep -Ec "$diagnostic_pattern" <<<"$aggregate_output" || true)
if [[ "$aggregate_status" -ne 1 ]]; then
	fail "aggregate expected exit 1, got $aggregate_status: $aggregate_output"
fi
if [[ "$aggregate_count" -ne "$total_expected" ]]; then
	fail "aggregate expected $total_expected diagnostics, got $aggregate_count"
fi

mkdir -p "$work/cross_file"
cat >"$work/cross_file/imports.rs" <<'RUST'
use sqlx::query as db_query;
use sqlx::query_as;
use sqlx::query_builder as qb;
use sqlx as db;
RUST
cat >"$work/cross_file/other.rs" <<'RUST'
fn names_do_not_leak_between_files() {
    let _ = db_query("not imported here");
    let _ = query_as("not imported here");
    let _ = db::query("not imported here");
    let _ = qb::QueryBuilder::new("not imported here");
}
RUST
expect_clean cross_file "$work/cross_file"

mkdir -p "$work/clean/tests"
cat >"$work/clean/clean.rs" <<'RUST'
fn near_misses() {
    let _ = query("not sqlx");
    let _ = query_with("not sqlx", args());
    let _ = other::query_unchecked! { "not sqlx" };
    let _ = other::QueryBuilder::with_arguments("not sqlx", args());
    let _ = other::query_builder::QueryBuilder::new("not sqlx");
    let _ = query_builder::QueryBuilder::new("not imported");
    let _ = r#type::QueryBuilder::new("raw alias not imported");
    let _ = sqlx_query("not sqlx");
}
RUST
cat >"$work/clean/fixture_test.rs" <<'RUST'
fn sibling_test_fixture() {
    let _ = sqlx::query_with("allowed in sibling tests", args());
    let _ = sqlx::query_unchecked! { "allowed in sibling tests" };
}
RUST
cat >"$work/clean/tests/integration.rs" <<'RUST'
fn integration_fixture() {
    let _ = sqlx::QueryBuilder::with_arguments("allowed in integration tests", args());
}
RUST
expect_clean clean "$work/clean"

if [[ "$failures" -gt 0 ]]; then
	echo "check-control-plane-sql-boundary-selftest: $failures failure(s)." >&2
	exit 1
fi

echo "check-control-plane-sql-boundary-selftest: OK"
