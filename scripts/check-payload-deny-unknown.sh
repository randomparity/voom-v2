#!/usr/bin/env bash
# Guard the durable-payload schema-evolution contract defined by ADR 0013.
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

# Keep shape selection structural; only classify each item's own source region.
# Capture scanner status outside process substitution so tool failures propagate.
rule="$rule_named_struct"$'\n---\n'"$rule_enum_inline_struct_variant"
scan_status=0
scan_output=$(ast-grep scan --inline-rules "$rule" --json=stream "${scope[@]}" 2>/dev/null) || scan_status=$?
if [[ "$scan_status" -gt 1 || ("$scan_status" -eq 1 && -z "$scan_output") ]]; then
	echo "check-payload-deny-unknown: ast-grep scan failed. Check the installed tool with 'just setup'." >&2
	exit 2
fi

matches=""
while IFS= read -r jline; do
	[[ -z "$jline" ]] && continue
	if [[ "$jline" =~ \"file\":\"([^\"]*)\" ]]; then
		file=${BASH_REMATCH[1]}
	else
		echo "check-payload-deny-unknown: could not parse ast-grep file" >&2
		exit 2
	fi
	if [[ "$jline" =~ \"start\":\{\"line\":([0-9]+) ]]; then
		line=$((BASH_REMATCH[1] + 1))
	else
		echo "check-payload-deny-unknown: could not parse ast-grep line" >&2
		exit 2
	fi
	if [[ "$jline" =~ \"ruleId\":\"(named-struct|enum-inline-struct-variant)\" ]]; then
		matches+="${BASH_REMATCH[1]}|$file:$line"$'\n'
	else
		echo "check-payload-deny-unknown: could not parse ast-grep rule" >&2
		exit 2
	fi
done <<<"$scan_output"
matches=$(printf '%s' "$matches" | sort -u) || {
	echo "check-payload-deny-unknown: could not sort ast-grep matches" >&2
	exit 2
}

# POSIX awk caches each scoped source in one pass. Attribute association retains
# the old contiguous upward block and inclusive 41-line downward header limit;
# multiline serde handling remains the separate correctness work in issue #259.
processor_status=0
awk '
BEGIN {
    for (i = 2; i < ARGC; i++) {
        original = ARGV[i]
        if (original ~ /^[[:alpha:]_][[:alnum:]_]*=/) ARGV[i] = "./" original
        path_count[ARGV[i]]++
        paths[ARGV[i], path_count[ARGV[i]]] = original
    }
}
function diagnose(file, line, message) {
    print "check-payload-deny-unknown: " file ":" line " — " message > "/dev/stderr"
    errors++
}
function item_region(file, line, n, text, region) {
    region = ""
    for (n = line - 1; n >= 1; n--) {
        text = source[file, n]
        if (text !~ /^[[:space:]]*(#\[|\/\/|$)/) break
        region = region text "\n"
    }
    for (n = line; n <= line + 40; n++) {
        text = source[file, n]
        if (text == "" && n > line) break
        region = region text "\n"
        if (index(text, "{")) break
    }
    return region
}
function classify(kind, file, line, region, lines, count, i, exempt, derives, deny, tag) {
    region = item_region(file, line)
    count = split(region, lines, "\n")
    for (i = 1; i <= count; i++) {
        if (lines[i] ~ /^[[:space:]]*\/\/[[:space:]]*payload-contract: exempt/) exempt = 1
        if (lines[i] ~ /^[[:space:]]*#\[derive\(/ && index(lines[i], "Deserialize")) derives = 1
        if (lines[i] ~ /^[[:space:]]*#\[serde\(/) {
            if (index(lines[i], "deny_unknown_fields")) deny = 1
            if (lines[i] ~ /(^|[^[:alnum:]_])tag([^[:alnum:]_]|$)/) tag = 1
        }
    }
    if (exempt) return
    if (kind == "named-struct" && derives && !deny) {
        diagnose(file, line, "Deserialize struct missing #[serde(deny_unknown_fields)]")
        print "  Add the attribute, or mark '\''// payload-contract: exempt — <reason>'\''. See docs/adr/0013." > "/dev/stderr"
    }
    if (kind == "enum-inline-struct-variant" && tag) {
        diagnose(file, line, "tagged enum has an inline struct-variant")
        print "  Extract each variant'\''s content to a named struct (newtype variant) — deny_unknown_fields is a no-op on inline variants — or mark '\''// payload-contract: exempt — <reason>'\''. See docs/adr/0013." > "/dev/stderr"
    }
}
FILENAME == "-" {
    if ($0 == "") next
    separator = index($0, "|")
    kind = substr($0, 1, separator - 1)
    hit = substr($0, separator + 1)
    file = hit
    sub(/:[0-9]+$/, "", file)
    line = hit
    sub(/^.*:/, "", line)
    count[kind]++
    files[kind, count[kind]] = file
    anchors[kind, count[kind]] = line + 0
    next
}
{
    if (FNR == 1) read_count[FILENAME]++
    file = paths[FILENAME, read_count[FILENAME]]
    source[file, FNR] = $0
    lengths[file] = FNR
    if ($0 ~ /#\[derive\($/) {
        diagnose(file, FNR, "multi-line #[derive(...)] is unsupported; keep it single-line")
    }
}
END {
    for (pass = 1; pass <= 2; pass++) {
        kind = pass == 1 ? "named-struct" : "enum-inline-struct-variant"
        for (i = 1; i <= count[kind]; i++) {
            file = files[kind, i]
            line = anchors[kind, i]
            if (line < 1 || line > lengths[file]) {
                print "check-payload-deny-unknown: ast-grep location does not resolve" > "/dev/stderr"
                exit 2
            }
            classify(kind, file, line)
        }
    }
    if (errors) {
        print "check-payload-deny-unknown: " errors " violation(s)." > "/dev/stderr"
        exit 1
    }
    print "check-payload-deny-unknown: OK"
}
' - "${scope[@]}" <<<"$matches" || processor_status=$?
if [[ "$processor_status" -gt 1 ]]; then
	echo "check-payload-deny-unknown: source processing failed. Check awk and scoped source files." >&2
	exit 2
fi
exit "$processor_status"
