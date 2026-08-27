#!/usr/bin/env bash
# Self-test for check-transaction-openers.sh. Lays out crate-shaped fixture
# trees in a throwaway root, runs the real guard, and asserts its exit code.
# Wired into `just ci` so the guard's ast-grep rule cannot silently rot.
#
# Every fixture is asserted in both directions: the behavior holds, and a
# fixture inverted to remove the hazard flips the verdict. A guard that only
# ever sees violations proves nothing about what it lets through.

set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
check="$script_dir/check-transaction-openers.sh"

failures=0
checks=0

# fixture <name> <expected-exit> <relative-path-under-root> <rust-source>
#
# The path is relative to the fake `crates/` root, so a fixture can place itself
# in an excluded location (a test file, the opener module) and prove the
# exclusion works.
fixture() {
	local name="$1" want="$2" rel="$3" body="$4" companion="${5-}"
	local work got=0
	work=$(mktemp -d)
	mkdir -p "$work/crates/$(dirname "$rel")"
	printf '%s\n' "$body" >"$work/crates/$rel"
	# An exclusion fixture places its only source in an excluded location, which
	# would leave no production sources at all -- and the guard rightly exits 2
	# on an empty tree. A clean companion keeps the tree non-empty, so the
	# assertion is "the excluded opener was not reported" rather than "nothing
	# was scanned".
	if [[ -n "$companion" ]]; then
		mkdir -p "$work/crates/voom-store/src/repo"
		printf '%s\n' "$companion" >"$work/crates/voom-store/src/repo/clean.rs"
	fi
	("$check" "$work/crates" >"$work/out" 2>&1) || got=$?
	checks=$((checks + 1))
	if [[ "$got" -ne "$want" ]]; then
		echo "FAIL [$name]: expected exit $want, got $got" >&2
		sed 's/^/    /' "$work/out" >&2
		failures=$((failures + 1))
	fi
	rm -rf "$work"
}

# --- The hazard: a pool-level opener outside the helper module ---

fixture direct_begin 1 'voom-store/src/repo/thing.rs' '
impl Repo {
    async fn f(&self) -> R {
        let mut tx = self.pool.begin().await?;
        tx.commit().await?;
        Ok(())
    }
}'

fixture direct_begin_with 1 'voom-store/src/repo/thing.rs' '
impl Repo {
    async fn f(&self) -> R {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        tx.commit().await?;
        Ok(())
    }
}'

# Most openers in this codebase are written as a multi-line chain. A line-based
# matcher misses these; the whole reason the guard uses ast-grep.
fixture multiline_chain 1 'voom-store/src/repo/thing.rs' '
impl Repo {
    async fn f(&self) -> R {
        let mut tx = self
            .pool
            .begin()
            .await?;
        tx.commit().await?;
        Ok(())
    }
}'

fixture bare_pool_local 1 'voom-store/src/repo/thing.rs' '
async fn f(pool: &SqlitePool) -> R {
    let mut tx = pool.begin().await?;
    tx.commit().await?;
    Ok(())
}'

# --- Inverted: the same code, opened the sanctioned way ---

fixture helper_call 0 'voom-store/src/repo/thing.rs' '
use crate::tx::begin_read_then_write;
impl Repo {
    async fn f(&self) -> R {
        let mut tx = begin_read_then_write(&self.pool, "thing: f").await?;
        tx.commit().await?;
        Ok(())
    }
}'

fixture helper_call_write_first 0 'voom-store/src/repo/thing.rs' '
use crate::tx::begin_write_first;
impl Repo {
    async fn f(&self) -> R {
        let mut tx = begin_write_first(&self.pool, "thing: f").await?;
        tx.commit().await?;
        Ok(())
    }
}'

# --- A savepoint is not a pool-level opener ---
#
# `tx.begin()` on a live handle nests inside an existing transaction. It cannot
# upgrade a pool lock, so it is not this rule's business. If the receiver
# constraint were dropped, this fixture would start failing.

fixture savepoint 0 'voom-store/src/repo/thing.rs' '
impl Repo {
    async fn f(&self, tx: &mut Transaction<Sqlite>) -> R {
        let mut sp = tx.begin().await?;
        sqlx::query("UPDATE t SET v = 1").execute(&mut *sp).await?;
        sp.commit().await?;
        Ok(())
    }
}'

# ...and neither is a savepoint on a handle the function owns outright.
fixture savepoint_owned_handle 0 'voom-store/src/repo/thing.rs' '
impl Repo {
    async fn f(mut tx: Transaction<Sqlite>) -> R {
        let mut sp = tx.begin().await?;
        sp.commit().await?;
        Ok(())
    }
}'

# --- Exclusions ---

# The helper module is the one place a bare opener belongs.
fixture opener_module 0 'voom-store/src/tx.rs' '
pub async fn begin_write_first(pool: &SqlitePool, context: &str) -> R {
    pool.begin().await.map_err(|e| VoomError::database_context(context, e))
}
pub async fn begin_read_then_write(pool: &SqlitePool, context: &str) -> R {
    pool.begin_with("BEGIN IMMEDIATE").await.map_err(|e| VoomError::database_context(context, e))
}' \
	'use crate::tx::begin_read_only;
impl Repo {
    async fn clean(&self) -> R {
        let mut tx = begin_read_only(&self.pool, "clean").await?;
        Ok(())
    }
}'

# Tests open transactions directly to build fixtures and to contend on purpose
# -- the contention tests in this very change do exactly that.
fixture sibling_test_source 0 'voom-store/src/repo/thing_test.rs' '
#[tokio::test]
async fn contends() {
    let writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let blocked = pool.begin().await;
}' \
	'use crate::tx::begin_read_only;
impl Repo {
    async fn clean(&self) -> R {
        let mut tx = begin_read_only(&self.pool, "clean").await?;
        Ok(())
    }
}'

fixture integration_test_source 0 'voom-store/tests/it.rs' '
#[tokio::test]
async fn contends() {
    let writer = pool.begin().await.unwrap();
}' \
	'use crate::tx::begin_read_only;
impl Repo {
    async fn clean(&self) -> R {
        let mut tx = begin_read_only(&self.pool, "clean").await?;
        Ok(())
    }
}'

# A `#[cfg(test)] mod tests;` module lives in `tests.rs`, which is a test file
# despite not matching `*_test.rs`.
fixture cfg_test_module 0 'voom-store/src/repo/artifacts/tests.rs' '
use super::*;
#[tokio::test]
async fn seeds() {
    let mut tx = pool.begin().await.unwrap();
}' \
	'use crate::tx::begin_write_first;
impl Repo {
    async fn clean(&self) -> R {
        let mut tx = begin_write_first(&self.pool, "clean").await?;
        Ok(())
    }
}'

# Support crates exist to serve tests.
fixture support_crate 0 'voom-test-support/src/lib.rs' '
pub async fn seed(pool: &SqlitePool) -> R {
    let mut tx = pool.begin().await?;
    tx.commit().await?;
    Ok(())
}' \
	'use crate::tx::begin_read_only;
impl Repo {
    async fn clean(&self) -> R {
        let mut tx = begin_read_only(&self.pool, "clean").await?;
        Ok(())
    }
}'

# --- Anti-vacuity ---
#
# A boundary check has exactly one silent failure mode: the rule stops matching
# and every run reports a clean tree. Nothing inside the rule can detect that,
# so it is asserted from outside.

# A file ast-grep cannot parse yields no matches, and no matches reads as clean.
fixture unparseable_file 2 'voom-store/src/repo/thing.rs' '
impl Repo {
    async fn f(&self) -> R {
        let mut tx = self.pool.begin().await?;
    // deliberately unbalanced from here
'

# The guard must fail when handed a root with no production sources rather than
# reporting success over nothing.
checks=$((checks + 1))
empty_root=$(mktemp -d)
mkdir -p "$empty_root/crates"
empty_got=0
("$check" "$empty_root/crates" >"$empty_root/out" 2>&1) || empty_got=$?
if [[ "$empty_got" -ne 2 ]]; then
	echo "FAIL [empty_root]: expected exit 2 over an empty tree, got $empty_got" >&2
	failures=$((failures + 1))
fi
rm -rf "$empty_root"

# The live rule still matches the shape it exists to catch. `direct_begin` above
# covers this for a synthetic tree; this covers the real one, so a rule that
# rots against the actual codebase is caught even if every fixture still passes.
checks=$((checks + 1))
probe=$(mktemp -d)
mkdir -p "$probe/crates/voom-store/src/repo"
cat >"$probe/crates/voom-store/src/repo/probe.rs" <<'RS'
impl Repo {
    async fn probe(&self) -> R {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await?;
        tx.commit().await?;
        Ok(())
    }
}
RS
probe_got=0
("$check" "$probe/crates" >"$probe/out" 2>&1) || probe_got=$?
if [[ "$probe_got" -ne 1 ]]; then
	echo "FAIL [matcher_alive]: the rule no longer matches a known pool-level opener" >&2
	sed 's/^/    /' "$probe/out" >&2
	failures=$((failures + 1))
fi
rm -rf "$probe"

# --------------------------------------------------------------------------

if [[ "$failures" -ne 0 ]]; then
	echo "check-transaction-openers-selftest: $failures of $checks checks failed" >&2
	exit 1
fi
echo "check-transaction-openers-selftest: OK ($checks checks)"
