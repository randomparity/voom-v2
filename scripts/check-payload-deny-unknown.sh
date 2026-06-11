#!/usr/bin/env bash
# Guard the durable-payload schema-evolution contract (audit M4, ADR 0013).
#
# For every source file listed in the scope file (default
# scripts/payload-contract-scope.txt), fail when:
#   1. A `Deserialize`-deriving named-field struct lacks
#      `#[serde(deny_unknown_fields)]` and is not preceded by an inline
#      `// payload-contract: exempt — <reason>` marker.
#   2. A `Deserialize`-deriving tagged enum (`#[serde(tag = ...)]`) has an inline
#      struct-variant carrying fields directly — the attribute is a silent no-op
#      there, so each variant's content must be a separate struct (newtype
#      variant) covered by rule 1.
#
# Tuple/newtype structs (no named fields) and unit-variant enums carry no
# field-drop surface and are ignored. Uses ast-grep (syntax-tree items, not
# text), like check-test-layout.sh and check-paused-time-db.sh.
#
# See docs/adr/0013-payload-evolution-contract.md and
# docs/payload-contract-inventory.md.

set -euo pipefail

if ! command -v ast-grep >/dev/null; then
	echo "check-payload-deny-unknown: ast-grep is required. Run 'just setup' to install." >&2
	exit 2
fi

scope_file="${PAYLOAD_CONTRACT_SCOPE:-scripts/payload-contract-scope.txt}"
if [[ ! -f "$scope_file" ]]; then
	echo "check-payload-deny-unknown: scope file not found: $scope_file" >&2
	exit 2
fi

# Read scope into an array (bash 3.2: read loop, not mapfile). Skip blanks/#.
scope=()
while IFS= read -r line; do
	case "$line" in '' | \#*) continue ;; esac
	if [[ ! -f "$line" ]]; then
		echo "check-payload-deny-unknown: scoped path does not resolve: $line" >&2
		exit 2
	fi
	scope+=("$line")
done <"$scope_file"

if [[ "${#scope[@]}" -eq 0 ]]; then
	echo "check-payload-deny-unknown: OK (empty scope)"
	exit 0
fi

# Position-only rules: locate items by SHAPE, never by binding an attribute via
# `follows`. (`follows: { stopBy: end }` traverses ALL preceding nodes, so it can
# match an *earlier* item's attribute — e.g. a missing-deny struct placed after a
# has-deny struct would be wrongly counted as covered. That cross-item shadowing
# is the normal mid-sweep state in the 14–17-struct payload files, so attributes
# are bound per-item by scanning each item's OWN text region below.)

# Every named-field struct (brace body excludes tuple/newtype structs).
# shellcheck disable=SC2016 # $ here is ast-grep meta-syntax, not a shell variable.
rule_named_struct='
id: named-struct
language: rust
severity: error
rule:
  kind: struct_item
  has: { field: body, kind: field_declaration_list }
'

# Every enum that has at least one inline struct-variant (carries fields directly).
# shellcheck disable=SC2016 # $ here is ast-grep meta-syntax, not a shell variable.
rule_enum_inline_struct_variant='
id: enum-inline-struct-variant
language: rust
severity: error
rule:
  kind: enum_item
  has:
    kind: enum_variant
    stopBy: end
    has: { kind: field_declaration_list }
'

# Emit "file:line" (1-based) for every node a rule matches across the scope.
# `--json=stream` writes one JSON object per match, one per line. Per object we
# pull the "file" string and the FIRST `"start":{"line":N` (which is the match
# range's start; the leading "text" field carries no nested start.line), then
# convert ast-grep's 0-based line to the 1-based line `sed`/editors use. Parsing
# per line keeps us independent of the object's field order (the field order is
# ast-grep-version-sensitive: in 0.42 "file" trails "range"). No jq dependency,
# same grep/sed discipline as check-paused-time-db.sh.
# `scan` exits non-zero when an error-severity rule matches; tolerate under set -e.
matches() {
	local rule="$1" jline file line0
	ast-grep scan --inline-rules "$rule" --json=stream "${scope[@]}" 2>/dev/null |
		while IFS= read -r jline; do
			file=$(printf '%s' "$jline" | grep -oE '"file":"[^"]*"' | head -1 | sed 's/^"file":"//; s/"$//')
			line0=$(printf '%s' "$jline" | grep -oE '"start":\{"line":[0-9]+' | head -1 | grep -oE '[0-9]+$')
			[[ -z "$file" || -z "$line0" ]] && continue
			printf '%s:%s\n' "$file" "$((line0 + 1))"
		done | sort -u || true
}

# The text region in which THIS item's attributes live, bound to the item alone
# (no cross-item shadowing): the contiguous attribute (`#[...]`) / line-comment
# (`//`) / blank block immediately ABOVE `line`, plus the item header from `line`
# down to the line that opens its body `{`. Covering both sides makes the scan
# robust to whether ast-grep anchors the match at the first attribute or at the
# struct/enum keyword. Assumes single-line `#[derive(...)]` (enforced below).
item_region() {
	local file="$1" line="$2" n text
	# Upward: contiguous attribute/comment/blank block.
	n=$((line - 1))
	while [[ "$n" -ge 1 ]]; do
		text=$(sed -n "${n}p" "$file")
		printf '%s\n' "$text" | grep -qE '^[[:space:]]*(#\[|//|$)' || break
		printf '%s\n' "$text"
		n=$((n - 1))
	done
	# Downward: item header through the line that opens the body.
	n="$line"
	while [[ "$n" -le "$((line + 40))" ]]; do
		text=$(sed -n "${n}p" "$file")
		[[ -z "$text" && "$n" -gt "$line" ]] && break
		printf '%s\n' "$text"
		printf '%s\n' "$text" | grep -q '{' && break
		n=$((n + 1))
	done
}

# True when the item's region carries a genuine exemption marker: a line-comment
# whose LEADING content is `payload-contract: exempt`. Anchored so doc-comment
# (`///`) prose that merely mentions the phrase mid-sentence cannot exempt a
# struct (the escape-hatch leak). Both gates share this, so they cannot drift.
is_exempt() {
	printf '%s' "$1" | grep -qE '^[[:space:]]*//[[:space:]]*payload-contract: exempt'
}

errors=0

# Fail closed on multi-line `#[derive(` (open paren ends the line): the per-item
# region scan assumes single-line derives, so reject the unsupported shape loudly
# instead of risking a silent miss. (None exist in scope today; rustfmt keeps
# these inline.)
for f in "${scope[@]}"; do
	while IFS= read -r ml; do
		[[ -z "$ml" ]] && continue
		echo "check-payload-deny-unknown: $f:${ml%%:*} — multi-line #[derive(...)] is unsupported; keep it single-line" >&2
		errors=$((errors + 1))
	done < <(grep -nE '#\[derive\($' "$f" 2>/dev/null || true)
done

# Rule 1: a named-field struct that derives Deserialize must carry
# deny_unknown_fields (or an exemption marker).
while IFS= read -r hit; do
	[[ -z "$hit" ]] && continue
	file="${hit%%:*}"
	line="${hit##*:}"
	region=$(item_region "$file" "$line")
	is_exempt "$region" && continue
	# Gate on the `#[derive(...)]` line, not the whole region: a Serialize-only
	# struct whose doc comment says "Deserialize" must not be treated as a
	# Deserialize struct.
	printf '%s' "$region" | grep -E '^[[:space:]]*#\[derive\(' | grep -q 'Deserialize' || continue
	# Gate on a `#[serde(...)]` line so a comment mentioning the attribute cannot
	# count as coverage.
	printf '%s' "$region" | grep -E '^[[:space:]]*#\[serde\(' | grep -q 'deny_unknown_fields' && continue
	echo "check-payload-deny-unknown: $file:$line — Deserialize struct missing #[serde(deny_unknown_fields)]" >&2
	echo "  Add the attribute, or mark '// payload-contract: exempt — <reason>'. See docs/adr/0013." >&2
	errors=$((errors + 1))
done < <(matches "$rule_named_struct")

# Rule 2: a tagged enum (serde tag) must not use inline struct-variants
# (deny_unknown_fields is a no-op there) — extract each to a newtype struct. Plain
# (untagged) enums with struct variants are normal Rust and are NOT flagged, so
# the `serde(tag` test is bound to the enum's own region, not a preceding item.
while IFS= read -r hit; do
	[[ -z "$hit" ]] && continue
	file="${hit%%:*}"
	line="${hit##*:}"
	region=$(item_region "$file" "$line")
	is_exempt "$region" && continue
	# Gate on a `#[serde(...)]` line carrying `tag` as a whole word: a plain enum
	# whose comment mentions serde(tag must not be flagged, `untagged` must not
	# match, and the attribute is found regardless of key order
	# (`#[serde(rename_all = "...", tag = "...")]`).
	printf '%s' "$region" | grep -E '^[[:space:]]*#\[serde\(' | grep -qE '\btag\b' || continue
	echo "check-payload-deny-unknown: $file:$line — tagged enum has an inline struct-variant" >&2
	echo "  Extract each variant's content to a named struct (newtype variant) — deny_unknown_fields is a no-op on inline variants — or mark '// payload-contract: exempt — <reason>'. See docs/adr/0013." >&2
	errors=$((errors + 1))
done < <(matches "$rule_enum_inline_struct_variant")

if [[ "$errors" -gt 0 ]]; then
	echo "check-payload-deny-unknown: $errors violation(s)." >&2
	exit 1
fi

echo "check-payload-deny-unknown: OK"
