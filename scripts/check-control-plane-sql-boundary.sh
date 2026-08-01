#!/usr/bin/env bash
# Enforce the architectural boundary that production control-plane code never
# constructs SQL. SQL and durable-row decoding belong in typed voom-store
# repository methods; sibling and integration test files may use SQL fixtures.

set -euo pipefail

if ! command -v ast-grep >/dev/null; then
	echo "check-control-plane-sql-boundary: ast-grep is required. Run 'just setup' to install." >&2
	exit 2
fi

root="${1:-crates/voom-control-plane/src}"
if [[ ! -d "$root" ]]; then
	echo "check-control-plane-sql-boundary: source root not found: $root" >&2
	exit 2
fi

source_list=$(find "$root" -type f -name '*.rs' \
	! -name '*_test.rs' ! -path '*/tests/*' | sort)

source_files=()
while IFS= read -r source_file; do
	[[ -z "$source_file" ]] && continue
	source_files+=("$source_file")
done <<<"$source_list"

if [[ "${#source_files[@]}" -eq 0 ]]; then
	echo "control-plane SQL boundary: OK"
	exit 0
fi

# SQLx 0.8.6's documented top-level SQL constructors and query macros.
function_names=(
	query query_with query_as query_as_with query_scalar query_scalar_with raw_sql
)
macro_names=(
	query query_unchecked
	query_as query_as_unchecked
	query_scalar query_scalar_unchecked
	query_file query_file_unchecked
	query_file_as query_file_as_unchecked
	query_file_scalar query_file_scalar_unchecked
)
builder_methods=(new with_arguments)
import_names=(
	query query_with query_unchecked
	query_as query_as_with query_as_unchecked
	query_scalar query_scalar_with query_scalar_unchecked
	query_file query_file_unchecked
	query_file_as query_file_as_unchecked
	query_file_scalar query_file_scalar_unchecked
	raw_sql query_builder QueryBuilder
)

violations=()

record_json_matches() {
	local api="$1"
	local output="$2"
	local json_line file line_zero
	while IFS= read -r json_line; do
		[[ -z "$json_line" ]] && continue
		if [[ "$json_line" =~ \"file\":\"([^\"]*)\" ]]; then
			file=${BASH_REMATCH[1]}
		else
			file=""
		fi
		if [[ "$json_line" =~ \"start\":\{\"line\":([0-9]+) ]]; then
			line_zero=${BASH_REMATCH[1]}
		else
			line_zero=""
		fi
		if [[ -z "$file" || -z "$line_zero" ]]; then
			echo "check-control-plane-sql-boundary: could not parse ast-grep output" >&2
			exit 2
		fi
		violations+=("$file|$((line_zero + 1))|$api")
	done <<<"$output"
}

parse_json_match() {
	local json="$1"
	if [[ "$json" =~ \"file\":\"([^\"]*)\" ]]; then
		match_file=${BASH_REMATCH[1]}
	else
		match_file=""
	fi
	if [[ "$json" =~ \"text\":\"([^\"]*)\" ]]; then
		match_text=${BASH_REMATCH[1]}
	else
		match_text=""
	fi
	if [[ -z "$match_file" || -z "$match_text" ]]; then
		echo "check-control-plane-sql-boundary: could not parse ast-grep match" >&2
		exit 2
	fi
}

collect_pattern() {
	local api="$1"
	local pattern="$2"
	shift 2
	local output
	local scan_status=0
	set +e
	output=$(ast-grep run --lang rust --pattern "$pattern" --json=stream "$@" 2>&1)
	scan_status=$?
	set -e
	if [[ "$scan_status" -gt 1 ]]; then
		echo "check-control-plane-sql-boundary: ast-grep failed for $api:" >&2
		echo "$output" >&2
		exit 2
	fi
	if [[ "$scan_status" -eq 0 ]]; then
		record_json_matches "$api" "$output"
	fi
}

scan_inline_rule() {
	local output_var="$1"
	local rule="$2"
	shift 2
	local output
	local scan_status=0
	set +e
	output=$(ast-grep scan --inline-rules "$rule" --json=stream "$@" 2>/dev/null)
	scan_status=$?
	set -e
	if [[ "$scan_status" -gt 1 ]]; then
		echo "check-control-plane-sql-boundary: ast-grep import scan failed" >&2
		exit 2
	fi
	printf -v "$output_var" '%s' "$output"
}

collect_function() {
	local qualifier="$1"
	local api="$2"
	shift 2
	collect_pattern "$api" "${qualifier}(\$\$\$ARGS)" "$@"
	collect_pattern "$api" "${qualifier}::<\$\$\$TYPES>(\$\$\$ARGS)" "$@"
}

collect_macro() {
	local qualifier="$1"
	local api="$2"
	shift 2
	collect_pattern "$api" "${qualifier}!(\$\$\$ARGS)" "$@"
	collect_pattern "$api" "${qualifier}!{\$\$\$ARGS}" "$@"
	collect_pattern "$api" "${qualifier}![\$\$\$ARGS]" "$@"
}

collect_builder() {
	local qualifier="$1"
	local api_prefix="$2"
	shift 2
	local method
	for method in "${builder_methods[@]}"; do
		collect_pattern "$api_prefix::$method" \
			"${qualifier}::$method(\$\$\$ARGS)" "$@"
		collect_pattern "$api_prefix::$method" \
			"${qualifier}::<\$\$\$TYPES>::$method(\$\$\$ARGS)" "$@"
	done
}

collect_namespace() {
	local namespace="$1"
	shift
	local name
	for name in "${function_names[@]}"; do
		collect_function "$namespace::$name" "sqlx::$name" "$@"
	done
	for name in "${macro_names[@]}"; do
		collect_macro "$namespace::$name" "sqlx::$name!" "$@"
	done
	collect_builder "$namespace::QueryBuilder" "sqlx::QueryBuilder" "$@"
	collect_builder "$namespace::query_builder::QueryBuilder" "sqlx::QueryBuilder" "$@"
}

collect_imported_item() {
	local local_name="$1"
	local original="$2"
	local file="$3"
	local name
	if [[ "$original" == "query_builder" ]]; then
		collect_builder "$local_name::QueryBuilder" "sqlx::QueryBuilder" "$file"
		return
	fi
	if [[ "$original" == "QueryBuilder" ]]; then
		collect_builder "$local_name" "sqlx::QueryBuilder" "$file"
		return
	fi
	for name in "${function_names[@]}"; do
		if [[ "$original" == "$name" ]]; then
			collect_function "$local_name" "sqlx::$original" "$file"
			break
		fi
	done
	for name in "${macro_names[@]}"; do
		if [[ "$original" == "$name" ]]; then
			collect_macro "$local_name" "sqlx::$original!" "$file"
			break
		fi
	done
}

collect_namespace sqlx "${source_files[@]}"
collect_namespace ::sqlx "${source_files[@]}"

# Discover unaliased imports structurally. The identifier must be inside a
# SQLx-rooted use declaration and not inside a use-as clause.
for import_name in "${import_names[@]}"; do
	import_rule=''
	import_rule+='id: sqlx-import'
	import_rule+=$'\nlanguage: rust\nseverity: error\nrule:\n  all:'
	import_rule+=$'\n    - kind: identifier\n    - regex: "^'
	import_rule+="$import_name"
	import_rule+='$"'
	import_rule+=$'\n    - inside:\n        kind: use_declaration'
	import_rule+=$'\n        regex: "(?s)\\\\buse\\\\s+(?:::)?sqlx\\\\s*::"'
	import_rule+=$'\n        stopBy: end\n    - not:\n        inside:'
	import_rule+=$'\n          kind: use_as_clause\n          stopBy: end\n'
	import_output=""
	scan_inline_rule import_output "$import_rule" "${source_files[@]}"
	while IFS= read -r import_match; do
		[[ -z "$import_match" ]] && continue
		parse_json_match "$import_match"
		collect_imported_item "$import_name" "$import_name" "$match_file"
	done <<<"$import_output"
done

IFS='|'
import_regex="${import_names[*]}"
unset IFS
alias_rule=''
alias_rule+='id: sqlx-item-alias'
alias_rule+=$'\nlanguage: rust\nseverity: error\nrule:\n  all:'
alias_rule+=$'\n    - kind: use_as_clause\n    - regex: "^(?:(?:::)?sqlx::)?('
alias_rule+="$import_regex"
alias_rule+=')\\s+as\\s+"'
alias_rule+=$'\n    - has:\n        field: alias\n        kind: identifier'
alias_rule+=$'\n    - inside:\n        kind: use_declaration'
alias_rule+=$'\n        regex: "(?s)\\\\buse\\\\s+(?:::)?sqlx(?:\\\\s+as|\\\\s*::)"'
alias_rule+=$'\n        stopBy: end\n'
alias_output=""
scan_inline_rule alias_output "$alias_rule" "${source_files[@]}"
while IFS= read -r alias_match; do
	[[ -z "$alias_match" ]] && continue
	parse_json_match "$alias_match"
	original=${match_text%% as *}
	original=${original##*::}
	alias=${match_text##* as }
	collect_imported_item "$alias" "$original" "$match_file"
done <<<"$alias_output"

# A renamed SQLx crate qualifies every constructor below it. `self as name`
# covers the grouped form; extern-crate aliases remain valid in this edition.
crate_alias_rule='
id: sqlx-crate-alias
language: rust
severity: error
rule:
  all:
    - kind: use_as_clause
    - regex: "^(?:(?:::)?sqlx|self)\\s+as\\s+"
    - has:
        field: alias
        kind: identifier
    - inside:
        kind: use_declaration
        regex: "(?s)\\buse\\s+(?:::)?sqlx(?:\\s+as|\\s*::)"
        stopBy: end
'
crate_alias_output=""
scan_inline_rule crate_alias_output "$crate_alias_rule" "${source_files[@]}"
while IFS= read -r crate_alias_match; do
	[[ -z "$crate_alias_match" ]] && continue
	parse_json_match "$crate_alias_match"
	crate_alias=${match_text##* as }
	collect_namespace "$crate_alias" "$match_file"
done <<<"$crate_alias_output"

extern_alias_rule='
id: sqlx-extern-crate-alias
language: rust
severity: error
rule:
  all:
    - kind: extern_crate_declaration
    - regex: "(?s)\\bextern\\s+crate\\s+sqlx\\s+as\\s+"
    - has:
        field: alias
        kind: identifier
'
extern_alias_output=""
scan_inline_rule extern_alias_output "$extern_alias_rule" "${source_files[@]}"
while IFS= read -r extern_alias_match; do
	[[ -z "$extern_alias_match" ]] && continue
	parse_json_match "$extern_alias_match"
	extern_alias=${match_text%;}
	extern_alias=${extern_alias##* as }
	collect_namespace "$extern_alias" "$match_file"
done <<<"$extern_alias_output"

# Reject SQLx-rooted wildcards at the import node because a wildcard obscures
# which SQL constructors enter local scope.
wildcard_rule='
id: sqlx-wildcard-import
language: rust
severity: error
rule:
  kind: use_wildcard
  inside:
    kind: use_declaration
    regex: "(?s)\\buse\\s+(?:::)?sqlx\\s*::"
    stopBy: end
'
wildcard_output=""
scan_inline_rule wildcard_output "$wildcard_rule" "${source_files[@]}"
while IFS= read -r wildcard_match; do
	[[ -z "$wildcard_match" ]] && continue
	parse_json_match "$wildcard_match"
	wildcard_api="sqlx::*"
	if [[ "$match_text" == *query_builder* ]]; then
		wildcard_api="sqlx::query_builder::*"
	fi
	record_json_matches "$wildcard_api" "$wildcard_match"
done <<<"$wildcard_output"

if [[ "${#violations[@]}" -gt 0 ]]; then
	sorted_violations=$(printf '%s\n' "${violations[@]}" | sort -t '|' -k1,1 -k2,2n -k3,3 -u)
	while IFS='|' read -r file line api; do
		printf '%s:%s — forbidden %s. Move SQL into a typed voom-store repository method.\n' \
			"$file" "$line" "$api" >&2
	done <<<"$sorted_violations"
	exit 1
fi

echo "control-plane SQL boundary: OK"
