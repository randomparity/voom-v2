#!/usr/bin/env bash
# Self-test for check-payload-deny-unknown.sh. Lays out fixture source files in a
# throwaway tree with a scope file pointing at them, runs the real guard, and
# asserts its exit code per case. Wired into `just ci` so the guard's ast-grep
# rules cannot silently rot.

set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
check="$script_dir/check-payload-deny-unknown.sh"

failures=0

# expect_exit <expected-code> <fixture-body>
# Writes the body to a single source file, a scope file naming it, runs the guard.
expect_exit() {
	local want="$1" body="$2"
	local work
	work=$(mktemp -d)
	printf '%s\n' "$body" >"$work/fixture.rs"
	printf '%s\n' "$work/fixture.rs" >"$work/scope.txt"
	local got=0
	(PAYLOAD_CONTRACT_SCOPE="$work/scope.txt" "$check" >/dev/null 2>&1) || got=$?
	rm -rf "$work"
	if [[ "$got" -ne "$want" ]]; then
		echo "FAIL: expected exit $want, got $got for body:" >&2
		printf '%s\n' "$body" >&2
		failures=$((failures + 1))
	fi
}

# --- Violation (exit 1): Deserialize struct missing deny_unknown_fields ---
expect_exit 1 '#[derive(Deserialize)]
struct Bad { a: u32 }'

# --- Violation (exit 1): internally tagged enum with an inline struct-variant ---
expect_exit 1 '#[derive(Deserialize)]
#[serde(tag = "kind")]
enum BadEnum { Replace { retired: u64 } }'

# --- Clean (exit 0): struct with the attribute ---
expect_exit 0 '#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Good { a: u32 }'

# --- Clean (exit 0): justified exemption immediately preceding ---
expect_exit 0 '// payload-contract: exempt — fixture-only, never read from a column
#[derive(Deserialize)]
struct Exempted { a: u32 }'

# --- Clean (exit 0): tagged enum with newtype variants over covered structs ---
expect_exit 0 '#[derive(Deserialize)]
#[serde(tag = "kind")]
enum GoodEnum { Replace(ReplaceContent) }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceContent { retired: u64 }'

# --- Clean (exit 0): exempted inline tagged enum (escape hatch, parity w/ struct) ---
expect_exit 0 '// payload-contract: exempt — non-durable fixture enum
#[derive(Deserialize)]
#[serde(tag = "kind")]
enum ExemptEnum { Replace { retired: u64 } }'

# --- Clean (exit 0): tuple/newtype struct (no named fields) is ignored ---
expect_exit 0 '#[derive(Deserialize)]
struct Triple(u64, u64, u64);'

# --- Clean (exit 0): non-Deserialize struct is ignored ---
expect_exit 0 '#[derive(Debug, Clone)]
struct NotSerde { a: u32 }'

# --- REAL-SHAPE fixtures: the guard must work on production struct shapes ---

# Violation: doc comment between the derive and the struct, multi-derive line,
# pub, missing deny_unknown_fields (the dominant Event-payload shape).
expect_exit 1 '#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// A payload the audit log writes.
pub struct RealMissing {
    pub job_id: String,
    #[serde(default)]
    pub note: Option<String>,
}'

# Violation: rename_all present but deny_unknown_fields absent.
expect_exit 1 '#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RenamedMissing { retired_at: String }'

# Clean: multi-derive line, doc comment, pub, deny present below rename_all.
expect_exit 0 '/// Doc above the derive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RealPresent {
    pub job_id: String,
}'

# Clean: exemption marker sits ABOVE the derive, with a doc comment between
# derive and struct (exercises the contiguous upward scan, not line-1 only).
expect_exit 0 '// payload-contract: exempt — deserialized only from a test fixture
#[derive(Debug, Deserialize)]
/// Helper, never read from a column.
struct ExemptedWithDoc {
    a: u32,
}'

# CROSS-ITEM SHADOWING (the iter-2 regression): a missing-deny struct positioned
# AFTER a has-deny struct in the same file MUST still be flagged. A guard that
# binds attributes via `follows: stopBy: end` would wrongly count the second as
# covered. This is the normal mid-sweep state of the 14–17-struct payload files.
expect_exit 1 '#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct First { a: u32 }
#[derive(Deserialize)]
struct SecondMissing { b: u32 }'

# Clean: a PLAIN (untagged) enum with a struct variant, positioned after a tagged
# enum, must NOT be flagged — the `serde(tag` test must bind to the enum itself,
# not a preceding item.
expect_exit 0 '#[derive(Deserialize)]
#[serde(tag = "kind")]
enum Tagged { A(AContent) }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AContent { x: u32 }
#[derive(Deserialize)]
enum PlainAfterTagged { Variant { y: u32 } }'

if [[ "$failures" -gt 0 ]]; then
	echo "check-payload-deny-unknown-selftest: $failures failure(s)." >&2
	exit 1
fi

echo "check-payload-deny-unknown-selftest: OK"
