#!/usr/bin/env bash
# Enforce that every pool-level transaction is opened by a named helper.
#
# Which BEGIN mode a transaction needs depends on what its first statement does.
# SQLite refuses a read->write lock upgrade with SQLITE_BUSY *without* invoking
# the busy handler, so a transaction that reads before it writes never consults
# busy_timeout and fails immediately under contention (issue #546). The author
# knows the shape while writing it; nothing downstream recovers it cheaply.
#
# So the shape is recorded in the opener's name, and this guard enforces that
# there is no other way to open one:
#
#   voom_store::tx::begin_read_then_write   BEGIN IMMEDIATE
#   voom_store::tx::begin_write_first       BEGIN
#   voom_store::tx::begin_read_only         BEGIN
#   voom_store::tx::begin_serialized_read   BEGIN IMMEDIATE
#
# Uses ast-grep (not ripgrep) so it matches real syntax-tree nodes: most openers
# in this codebase are multi-line `self\n.pool\n.begin()` chains that no line
# match sees, and a savepoint (`tx.begin()` on a live handle) must not be
# confused with a pool-level opener.
#
# See docs/adr/0086-transaction-openers-are-named-helpers.md,
# docs/adr/0083-read-then-write-transactions-begin-immediate.md, and AGENTS.md.

set -euo pipefail

if ! command -v ast-grep >/dev/null; then
	echo "check-transaction-openers: ast-grep is required. Run 'just setup' to install." >&2
	exit 2
fi

root="${1:-crates}"
if [[ ! -d "$root" ]]; then
	echo "check-transaction-openers: source root not found: $root" >&2
	exit 2
fi

# The one module allowed to call pool.begin*(): the helpers themselves.
opener_module="voom-store/src/tx.rs"

# Support crates exist to serve tests; production scope is the rest.
support_crates='voom-test-support|voom-fakes|voom-fake-support|voom-conformance'

source_files=()
while IFS= read -r source_file; do
	[[ -z "$source_file" ]] && continue
	source_files+=("$source_file")
done < <(find "$root" -type f -name '*.rs' \
	-path '*/src/*' \
	! -name '*_test.rs' \
	! -name 'tests.rs' \
	! -name 'test_support.rs' \
	! -path '*/tests/*' \
	! -path "*/$opener_module" |
	grep -Ev "/($support_crates)/" |
	sort)

if [[ "${#source_files[@]}" -eq 0 ]]; then
	echo "check-transaction-openers: no production sources found under $root" >&2
	exit 2
fi

# `constraints` is what separates a pool receiver from a live transaction
# handle: `tx.begin()` opens a savepoint, which cannot upgrade a pool lock and
# is not this rule's business.
# shellcheck disable=SC2016  # $POOL/$MODE are ast-grep metavariables, not shell
rule='
id: pool-opener
language: rust
severity: error
rule:
  any:
    - pattern: $POOL.begin()
    - pattern: $POOL.begin_with($$$MODE)
constraints:
  POOL:
    regex: "(?i)pool"
'

# Anti-vacuity, and it has to come first. A region ast-grep cannot parse becomes
# an ERROR node and yields no matches inside it -- which reads exactly like a
# clean file. tree-sitter error-recovers rather than failing, so nothing on
# stderr says so; the ERROR nodes themselves are the only signal. A valid Rust
# file has none, so any hit means either genuinely broken source or an ast-grep
# grammar that has drifted from the Rust this repo compiles.
error_nodes=$(ast-grep scan --inline-rules '
id: parse-error
language: rust
severity: error
rule: { kind: ERROR }
' "${source_files[@]}" --json=stream 2>/dev/null || true)

if [[ -n "$error_nodes" ]]; then
	echo "check-transaction-openers: ast-grep could not parse these files:" >&2
	echo "$error_nodes" | python3 -c '
import json, sys
seen = set()
for line in sys.stdin:
    if not line.strip():
        continue
    f = json.loads(line)["file"]
    if f not in seen:
        seen.add(f)
        print("  {}".format(f))
' >&2
	echo "  An unparsed region yields no matches, which would pass vacuously." >&2
	exit 2
fi

matches=$(ast-grep scan --inline-rules "$rule" "${source_files[@]}" \
	--json=stream 2>/dev/null || true)

if [[ -z "$matches" ]]; then
	echo "check-transaction-openers: OK (${#source_files[@]} files)"
	exit 0
fi

echo "check-transaction-openers: pool-level transactions opened outside voom_store::tx" >&2
echo "$matches" | python3 -c '
import json, sys
for line in sys.stdin:
    if not line.strip():
        continue
    m = json.loads(line)
    print("  {}:{}".format(m["file"], m["range"]["start"]["line"] + 1))
' >&2 2>/dev/null || echo "$matches" >&2

cat >&2 <<'MSG'

Open it with the helper that names what the transaction does:

  voom_store::tx::begin_read_then_write(&pool, "context")  reads, then writes
  voom_store::tx::begin_write_first(&pool, "context")      first statement writes
  voom_store::tx::begin_read_only(&pool, "context")        never writes
  voom_store::tx::begin_serialized_read(&pool, "context")  never writes, but must
                                                           not read a stale snapshot

The first statement executed against the handle is what decides -- including
statements inside the *_in_tx helpers the handle is passed to.

If it only reads, ask whether a concurrent writer's outcome matters. WAL readers
do not block on an in-flight writer, so a plain BEGIN can pass a guard on state
the next statement invalidates -- that is what begin_serialized_read is for.

See docs/adr/0083 and docs/adr/0086.
MSG
exit 1
