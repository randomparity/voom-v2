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
normal_imports=()
wildcard_imports=()
type_relations=()
resolved_crates=()
use_list_nodes=()
use_alias_nodes=()
use_leaf_nodes=()
resolved_use_lists=()
current_use_list_file=""
current_file_use_lists=()
use_list_load_index=0

scan_inline_rule() {
	local output_var="$1"
	local rule="$2"
	shift 2
	local scan_output
	local scan_status=0
	set +e
	scan_output=$(ast-grep scan --inline-rules "$rule" --json=stream "$@" 2>/dev/null)
	scan_status=$?
	set -e
	if [[ "$scan_status" -gt 1 ]]; then
		echo "check-control-plane-sql-boundary: ast-grep scan failed" >&2
		exit 2
	fi
	printf -v "$output_var" '%s' "$scan_output"
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
	if [[ "$json" =~ \"start\":\{\"line\":([0-9]+) ]]; then
		match_line=$((BASH_REMATCH[1] + 1))
	else
		match_line=""
	fi
	if [[ -z "$match_file" || -z "$match_text" || -z "$match_line" ]]; then
		echo "check-control-plane-sql-boundary: could not parse ast-grep output" >&2
		exit 2
	fi
}

parse_json_range() {
	local json="$1"
	if [[ "$json" =~ \"byteOffset\":\{\"start\":([0-9]+),\"end\":([0-9]+) ]]; then
		match_start=${BASH_REMATCH[1]}
		match_end=${BASH_REMATCH[2]}
	else
		echo "check-control-plane-sql-boundary: could not parse ast-grep range" >&2
		exit 2
	fi
}

parse_json_meta() {
	local json="$1"
	local name="$2"
	local marker="\"$name\":"
	local tail=${json#*"$marker"}
	if [[ "$tail" == "$json" || ! "$tail" =~ \"text\":\"([^\"]*)\" ]]; then
		echo "check-control-plane-sql-boundary: missing ast-grep metavariable $name" >&2
		exit 2
	fi
	meta_text=${BASH_REMATCH[1]}
	if [[ "$tail" =~ \"byteOffset\":\{\"start\":([0-9]+),\"end\":([0-9]+) ]]; then
		meta_start=${BASH_REMATCH[1]}
		meta_end=${BASH_REMATCH[2]}
	else
		echo "check-control-plane-sql-boundary: missing ast-grep metavariable range" >&2
		exit 2
	fi
}

normalize_ast_text() {
	normalized_text=${1//\\n/ }
	normalized_text=${normalized_text//\\t/ }
}

trim_value() {
	trimmed_value="$1"
	trimmed_value=${trimmed_value#"${trimmed_value%%[![:space:]]*}"}
	trimmed_value=${trimmed_value%"${trimmed_value##*[![:space:]]}"}
}

normalize_source() {
	trim_value "$1"
	normalized_source="$trimmed_value"
	while [[ "$normalized_source" == self::* ]]; do
		normalized_source=${normalized_source#self::}
	done
}

append_normal_import() {
	local file="$1"
	local source="$2"
	local target="$3"
	local line="$4"
	normalize_source "$source"
	trim_value "$target"
	normal_imports+=("$file|$normalized_source|$trimmed_value|$line")
}

parse_flat_use_match() {
	local file="$1"
	local line="$2"
	local text="$3"
	normalize_ast_text "$text"
	text=${normalized_text%;}
	text=${text#*use }
	trim_value "$text"
	text="$trimmed_value"
	[[ "$text" == *"{"* ]] && return 0
	if [[ "$text" == *"::*" ]]; then
		normalize_source "${text%::*}"
		wildcard_imports+=("$file|$normalized_source|$line")
	elif [[ "$text" == *" as "* ]]; then
		append_normal_import "$file" "${text%% as *}" "${text##* as }" "$line"
	else
		append_normal_import "$file" "$text" "${text##*::}" "$line"
	fi
}

load_use_tree_nodes() {
	local list_rule alias_rule leaf_rule output="" json_line
	list_rule=$'id: nested-use-list\nlanguage: rust\nseverity: error\nrule:\n  pattern:'
	list_rule+=$'\n    context: use $PATH::{$$$ITEMS};\n    selector: scoped_use_list'
	scan_inline_rule output "$list_rule" "$@"
	while IFS= read -r json_line; do
		[[ -z "$json_line" ]] && continue
		parse_json_match "$json_line"
		parse_json_range "$json_line"
		parse_json_meta "$json_line" PATH
		use_list_nodes+=(
			"$match_file|$match_start|$match_end|$meta_start|$meta_end|$meta_text|$match_line"
		)
	done <<<"$output"
	alias_rule=$'id: nested-use-alias\nlanguage: rust\nseverity: error\nrule:\n  all:'
	alias_rule+=$'\n    - pattern:\n        context: use $SOURCE as $ALIAS;'
	alias_rule+=$'\n        selector: use_as_clause\n    - inside:'
	alias_rule+=$'\n        kind: scoped_use_list\n        stopBy: end'
	output=""
	scan_inline_rule output "$alias_rule" "$@"
	while IFS= read -r json_line; do
		[[ -z "$json_line" ]] && continue
		parse_json_match "$json_line"
		parse_json_range "$json_line"
		parse_json_meta "$json_line" SOURCE
		local source="$meta_text"
		parse_json_meta "$json_line" ALIAS
		use_alias_nodes+=(
			"$match_file|$match_start|$match_end|$match_line|$source|$meta_text"
		)
	done <<<"$output"
	load_use_tree_leaves "$@"
}

load_use_tree_leaves() {
	local leaf_rule output="" json_line
	leaf_rule=$'id: nested-use-leaf\nlanguage: rust\nseverity: error\nrule:\n  all:\n    - any:'
	leaf_rule+=$'\n        - kind: use_wildcard\n        - all:\n            - kind: self'
	leaf_rule+=$'\n            - not:\n                inside:\n                  kind: use_as_clause'
	leaf_rule+=$'\n        - all:\n            - kind: scoped_identifier\n            - not:'
	leaf_rule+=$'\n                inside:\n                  kind: scoped_identifier'
	leaf_rule+=$'\n            - not:\n                inside:\n                  kind: use_wildcard'
	leaf_rule+=$'\n        - all:\n            - kind: identifier\n            - not:'
	leaf_rule+=$'\n                inside:\n                  kind: scoped_identifier'
	leaf_rule+=$'\n            - not:\n                inside:\n                  kind: use_as_clause'
	leaf_rule+=$'\n            - not:\n                inside:\n                  kind: use_wildcard'
	leaf_rule+=$'\n    - inside:\n        kind: scoped_use_list\n        stopBy: end'
	scan_inline_rule output "$leaf_rule" "$@"
	while IFS= read -r json_line; do
		[[ -z "$json_line" ]] && continue
		parse_json_match "$json_line"
		parse_json_range "$json_line"
		use_leaf_nodes+=("$match_file|$match_start|$match_end|$match_line|$match_text")
	done <<<"$output"
}

sort_use_list_nodes() {
	local entry
	local sorted_nodes=()
	local sorted_aliases=()
	local sorted_leaves=()
	if [[ "${#use_list_nodes[@]}" -gt 0 ]]; then
		while IFS= read -r entry; do
			[[ -n "$entry" ]] && sorted_nodes+=("$entry")
		done < <(printf '%s\n' "${use_list_nodes[@]}" | sort -t '|' -k1,1 -k2,2n -k3,3nr)
		use_list_nodes=("${sorted_nodes[@]}")
	fi
	if [[ "${#use_alias_nodes[@]}" -gt 0 ]]; then
		while IFS= read -r entry; do
			[[ -n "$entry" ]] && sorted_aliases+=("$entry")
		done < <(printf '%s\n' "${use_alias_nodes[@]}" | sort -t '|' -k1,1 -k2,2n)
		use_alias_nodes=("${sorted_aliases[@]}")
	fi
	if [[ "${#use_leaf_nodes[@]}" -gt 0 ]]; then
		while IFS= read -r entry; do
			[[ -n "$entry" ]] && sorted_leaves+=("$entry")
		done < <(printf '%s\n' "${use_leaf_nodes[@]}" | sort -t '|' -k1,1 -k2,2n)
		use_leaf_nodes=("${sorted_leaves[@]}")
	fi
}

resolve_use_list_paths() {
	local node file start end path_start path_end path line
	local parent_file parent_start parent_end parent_path
	local full_path parent_node index
	[[ "${#use_list_nodes[@]}" -gt 0 ]] || return 0
	for node in "${use_list_nodes[@]}"; do
		IFS='|' read -r file start end path_start path_end path line <<<"$node"
		parent_path=""
		for ((index = ${#resolved_use_lists[@]} - 1; index >= 0; index--)); do
			parent_node=${resolved_use_lists[$index]}
			IFS='|' read -r parent_file parent_start parent_end _ _ _ _ \
				parent_path_candidate <<<"$parent_node"
			[[ "$parent_file" == "$file" ]] || break
			[[ "$parent_start" -le "$start" && "$parent_end" -ge "$end" ]] || continue
			parent_path="$parent_path_candidate"
			break
		done
		full_path="$path"
		[[ -n "$parent_path" ]] && full_path="$parent_path::$path"
		resolved_use_lists+=(
			"$file|$start|$end|$path_start|$path_end|$path|$line|$full_path"
		)
	done
}

reset_file_use_list_cache() {
	current_use_list_file=""
	current_file_use_lists=()
	use_list_load_index=0
}

load_file_use_lists() {
	local file="$1"
	local node node_file
	[[ "$current_use_list_file" == "$file" ]] && return 0
	current_use_list_file="$file"
	current_file_use_lists=()
	[[ "${#resolved_use_lists[@]}" -gt 0 ]] || return 0
	while [[ "$use_list_load_index" -lt "${#resolved_use_lists[@]}" ]]; do
		node=${resolved_use_lists[$use_list_load_index]}
		IFS='|' read -r node_file _ <<<"$node"
		if [[ "$node_file" == "$file" ]]; then
			current_file_use_lists+=("$node")
		elif [[ "${#current_file_use_lists[@]}" -gt 0 ]]; then
			break
		fi
		use_list_load_index=$((use_list_load_index + 1))
	done
	return 0
}

find_containing_use_list() {
	local file="$1"
	local start="$2"
	local end="$3"
	local node node_start node_end path_start path_end prefix index
	containing_prefix=""
	containing_path_start=""
	containing_path_end=""
	load_file_use_lists "$file"
	[[ "${#current_file_use_lists[@]}" -gt 0 ]] || return 0
	for ((index = ${#current_file_use_lists[@]} - 1; index >= 0; index--)); do
		node=${current_file_use_lists[$index]}
		IFS='|' read -r _ node_start node_end path_start path_end _ _ prefix \
			<<<"$node"
		[[ "$node_start" -le "$start" && "$node_end" -ge "$end" ]] || continue
		containing_prefix="$prefix"
		containing_path_start="$path_start"
		containing_path_end="$path_end"
		return 0
	done
}

range_is_use_alias() {
	local file="$1"
	local start="$2"
	local end="$3"
	local node node_file alias_start alias_end
	[[ "${#use_alias_nodes[@]}" -gt 0 ]] || return 1
	for node in "${use_alias_nodes[@]}"; do
		IFS='|' read -r node_file alias_start alias_end _ <<<"$node"
		[[ "$node_file" == "$file" ]] || continue
		[[ "$start" -ge "$alias_start" && "$end" -le "$alias_end" ]] && return 0
	done
	return 1
}

process_nested_use_aliases() {
	local node file start end line source target
	[[ "${#use_alias_nodes[@]}" -gt 0 ]] || return 0
	reset_file_use_list_cache
	for node in "${use_alias_nodes[@]}"; do
		IFS='|' read -r file start end line source target <<<"$node"
		find_containing_use_list "$file" "$start" "$end"
		[[ -n "$containing_prefix" ]] || continue
		if [[ "$source" == self ]]; then
			source="$containing_prefix"
		else
			source="$containing_prefix::$source"
		fi
		append_normal_import "$file" "$source" "$target" "$line"
	done
}

process_nested_use_leaves() {
	local node file start end line text source
	[[ "${#use_leaf_nodes[@]}" -gt 0 ]] || return 0
	reset_file_use_list_cache
	for node in "${use_leaf_nodes[@]}"; do
		IFS='|' read -r file start end line text <<<"$node"
		find_containing_use_list "$file" "$start" "$end"
		[[ -n "$containing_prefix" ]] || continue
		if [[ "$start" -ge "$containing_path_start" && "$end" -le "$containing_path_end" ]]; then
			continue
		fi
		range_is_use_alias "$file" "$start" "$end" && continue
		if [[ "$text" == *"*"* ]]; then
			source="$containing_prefix"
			[[ "$text" != "*" ]] && source="$containing_prefix::${text%::*}"
			wildcard_imports+=("$file|$source|$line")
		elif [[ "$text" == self ]]; then
			append_normal_import \
				"$file" "$containing_prefix" "${containing_prefix##*::}" "$line"
		else
			append_normal_import \
				"$file" "$containing_prefix::$text" "${text##*::}" "$line"
		fi
	done
}

load_nested_use_inventory() {
	load_use_tree_nodes "$@"
	sort_use_list_nodes
	resolve_use_list_paths
	process_nested_use_aliases
	process_nested_use_leaves
}

load_import_inventory() {
	local import_rule=$'id: rust-import\nlanguage: rust\nseverity: error\nrule:\n  any:'
	import_rule+=$'\n    - kind: use_declaration\n    - kind: extern_crate_declaration'
	local output=""
	scan_inline_rule output "$import_rule" "$@"
	local json_line
	while IFS= read -r json_line; do
		[[ -z "$json_line" ]] && continue
		parse_json_match "$json_line"
		normalize_ast_text "$match_text"
		if [[ "$normalized_text" =~ crate[[:space:]]+sqlx[[:space:]]+as[[:space:]]+([^\;]+) ]]; then
			append_normal_import "$match_file" sqlx "${BASH_REMATCH[1]}" "$match_line"
		else
			parse_flat_use_match "$match_file" "$match_line" "$match_text"
		fi
	done <<<"$output"
	load_nested_use_inventory "$@"
}

file_has_resolved_crate() {
	local file="$1"
	local crate_name="$2"
	local entry
	[[ "${#resolved_crates[@]}" -gt 0 ]] || return 1
	for entry in "${resolved_crates[@]}"; do
		[[ "$entry" == "$file|$crate_name" ]] && return 0
	done
	return 1
}

record_structural_paths() {
	local output="$1"
	local kind="$2"
	local json_line crate_name terminal api
	while IFS= read -r json_line; do
		[[ -z "$json_line" ]] && continue
		parse_json_match "$json_line"
		parse_json_meta "$json_line" CRATE
		crate_name="$meta_text"
		[[ "$crate_name" == ::* ]] && crate_name="::sqlx"
		file_has_resolved_crate "$match_file" "$crate_name" || continue
		if [[ ! "$match_text" =~ ([A-Za-z_][A-Za-z0-9_]*)$ ]]; then
			echo "check-control-plane-sql-boundary: could not identify SQLx path" >&2
			exit 2
		fi
		terminal=${BASH_REMATCH[1]}
		if [[ "$kind" == macro ]]; then
			api="sqlx::$terminal!"
		else
			api="sqlx::$terminal"
		fi
		violations+=("$match_file|$match_line|$api")
	done <<<"$output"
}

load_surface_path_violations() {
	local rule output="" name
	rule=$'id: sqlx-surface-path\nlanguage: rust\nseverity: error\nrule:\n  all:'
	rule+=$'\n    - any:'
	for name in "${function_names[@]}" query_builder; do
		rule+=$'\n        - pattern:\n            context: $CRATE::'
		rule+="$name"
		rule+=$'\n            selector: scoped_identifier'
	done
	rule+=$'\n    - not:\n        inside:\n          kind: scoped_identifier'
	rule+=$'\n          stopBy: end\n    - not:\n        inside:'
	rule+=$'\n          kind: macro_invocation\n          stopBy: end'
	rule+=$'\n    - not:\n        inside:\n          kind: use_wildcard'
	rule+=$'\n          stopBy: end'
	rule+=$'\nconstraints:\n  CRATE:\n    any:\n      - kind: identifier'
	rule+=$'\n      - pattern: ::sqlx'
	scan_inline_rule output "$rule" "${source_files[@]}"
	record_structural_paths "$output" surface
}

load_builder_path_violations() {
	local rule output=""
	rule=$'id: sqlx-builder-path\nlanguage: rust\nseverity: error\nrule:\n  any:'
	rule+=$'\n    - pattern:\n        context: $CRATE::query_builder::QueryBuilder'
	rule+=$'\n        selector: scoped_identifier\n    - pattern:'
	rule+=$'\n        context: $CRATE::QueryBuilder'
	rule+=$'\n        selector: scoped_identifier'
	rule+=$'\nconstraints:\n  CRATE:\n    any:\n      - kind: identifier'
	rule+=$'\n      - pattern: ::sqlx'
	scan_inline_rule output "$rule" "${source_files[@]}"
	record_structural_paths "$output" builder
}

load_macro_path_violations() {
	local rule output="" name
	rule=$'id: sqlx-macro-path\nlanguage: rust\nseverity: error\nrule:\n  all:'
	rule+=$'\n    - any:'
	for name in "${macro_names[@]}"; do
		rule+=$'\n        - pattern:\n            context: $CRATE::'
		rule+="$name"
		rule+=$'\n            selector: scoped_identifier'
	done
	rule+=$'\n    - inside:\n        kind: macro_invocation\n        stopBy: end'
	rule+=$'\nconstraints:\n  CRATE:\n    any:\n      - kind: identifier'
	rule+=$'\n      - pattern: ::sqlx'
	scan_inline_rule output "$rule" "${source_files[@]}"
	record_structural_paths "$output" macro
}

load_type_relations() {
	local rule=$'id: rust-type-alias\nlanguage: rust\nseverity: error\nrule:\n  kind: type_item'
	local output=""
	scan_inline_rule output "$rule" "$@"
	local json_line source target
	local type_name_pattern='type[[:space:]]+(r#[A-Za-z_][A-Za-z0-9_]*|[A-Za-z_][A-Za-z0-9_]*)'
	while IFS= read -r json_line; do
		[[ -z "$json_line" ]] && continue
		parse_json_match "$json_line"
		normalize_ast_text "$match_text"
		if [[ ! "$normalized_text" =~ $type_name_pattern ]]; then
			continue
		fi
		target=${BASH_REMATCH[1]}
		source=${normalized_text#*=}
		source=${source%;}
		source=${source%%<*}
		normalize_source "$source"
		type_relations+=("$match_file|$match_line|$normalized_source|$target")
	done <<<"$output"
}

array_contains() {
	local needle="$1"
	shift
	local value
	for value in "$@"; do
		[[ "$value" == "$needle" ]] && return 0
	done
	return 1
}

set_import_api() {
	local category="$1"
	local canonical="$2"
	case "$category" in
	item)
		import_api="sqlx::$canonical"
		if ! array_contains "$canonical" "${function_names[@]}"; then
			import_api+="!"
		fi
		;;
	module) import_api='sqlx::query_builder' ;;
	builder) import_api='sqlx::QueryBuilder' ;;
	crate) import_api="" ;;
	esac
}

add_resolved() {
	local category="$1"
	local canonical="$2"
	local local_name="$3"
	local entry="$category|$canonical|$local_name"
	local existing name
	if [[ "${#resolved_entries[@]}" -gt 0 ]]; then
		for existing in "${resolved_entries[@]}"; do
			[[ "$existing" == "$entry" ]] && return
		done
	fi
	resolved_entries+=("$entry")
	if [[ "$category" == crate ]]; then
		local crate_entry="$current_file|$local_name"
		if [[ "${#resolved_crates[@]}" -eq 0 ]] ||
			! array_contains "$crate_entry" "${resolved_crates[@]}"; then
			resolved_crates+=("$crate_entry")
		fi
	fi
	case "$category" in
	crate)
		for name in "${import_names[@]}"; do
			case "$name" in
			query_builder) add_resolved module query_builder "$local_name::$name" ;;
			QueryBuilder) add_resolved builder QueryBuilder "$local_name::$name" ;;
			*) add_resolved item "$name" "$local_name::$name" ;;
			esac
		done
		;;
	module) add_resolved builder QueryBuilder "$local_name::QueryBuilder" ;;
	item | builder) ;;
	esac
}

process_import_relations() {
	local category="$1"
	local canonical="$2"
	local local_name="$3"
	local entry source target line api
	if [[ "${#current_imports[@]}" -gt 0 ]]; then
		for entry in "${current_imports[@]}"; do
			IFS='|' read -r source target line <<<"$entry"
			[[ "$source" == "$local_name" ]] || continue
			set_import_api "$category" "$canonical"
			[[ -n "$import_api" ]] && violations+=("$current_file|$line|$import_api")
			add_resolved "$category" "$canonical" "$target"
		done
	fi
	[[ "$category" == crate || "$category" == module ]] || return 0
	[[ "${#current_wildcards[@]}" -gt 0 ]] || return 0
	api='sqlx::*'
	[[ "$category" == module ]] && api='sqlx::query_builder::*'
	for entry in "${current_wildcards[@]}"; do
		IFS='|' read -r source line <<<"$entry"
		[[ "$source" == "$local_name" ]] || continue
		violations+=("$current_file|$line|$api")
	done
}

process_type_relations() {
	local category="$1"
	local local_name="$2"
	[[ "$category" == builder ]] || return 0
	[[ "${#current_types[@]}" -gt 0 ]] || return 0
	local entry line source target
	for entry in "${current_types[@]}"; do
		IFS='|' read -r line source target <<<"$entry"
		[[ "$source" == "$local_name" ]] || continue
		violations+=("$current_file|$line|sqlx::QueryBuilder")
		add_resolved builder QueryBuilder "$target"
	done
}

sort_inventory() {
	local entry
	sorted_inventory=()
	while IFS= read -r entry; do
		[[ -n "$entry" ]] && sorted_inventory+=("$entry")
	done < <(printf '%s\n' "$@" | sort)
}

load_current_relations() {
	local file_filter="$1"
	local entry file first second third
	current_imports=()
	current_wildcards=()
	current_types=()
	while [[ "$import_index" -lt "${#normal_imports[@]}" ]]; do
		entry=${normal_imports[$import_index]}
		IFS='|' read -r file first second third <<<"$entry"
		[[ "$file" == "$file_filter" ]] || break
		current_imports+=("$first|$second|$third")
		import_index=$((import_index + 1))
	done
	while [[ "$wildcard_index" -lt "${#wildcard_imports[@]}" ]]; do
		entry=${wildcard_imports[$wildcard_index]}
		IFS='|' read -r file first second <<<"$entry"
		[[ "$file" == "$file_filter" ]] || break
		current_wildcards+=("$first|$second")
		wildcard_index=$((wildcard_index + 1))
	done
	while [[ "$type_index" -lt "${#type_relations[@]}" ]]; do
		entry=${type_relations[$type_index]}
		IFS='|' read -r file first second third <<<"$entry"
		[[ "$file" == "$file_filter" ]] || break
		current_types+=("$first|$second|$third")
		type_index=$((type_index + 1))
	done
}

resolve_file() {
	current_file="$1"
	load_current_relations "$current_file"
	resolved_entries=()
	add_resolved crate sqlx sqlx
	add_resolved crate sqlx ::sqlx
	local index=0 entry category canonical local_name
	while [[ "$index" -lt "${#resolved_entries[@]}" ]]; do
		entry=${resolved_entries[$index]}
		IFS='|' read -r category canonical local_name <<<"$entry"
		process_import_relations "$category" "$canonical" "$local_name"
		process_type_relations "$category" "$local_name"
		index=$((index + 1))
	done
}

load_import_inventory "${source_files[@]}"
load_type_relations "${source_files[@]}"
if [[ "${#normal_imports[@]}" -gt 0 ]]; then
	sort_inventory "${normal_imports[@]}"
	normal_imports=("${sorted_inventory[@]}")
fi
if [[ "${#wildcard_imports[@]}" -gt 0 ]]; then
	sort_inventory "${wildcard_imports[@]}"
	wildcard_imports=("${sorted_inventory[@]}")
fi
if [[ "${#type_relations[@]}" -gt 0 ]]; then
	sort_inventory "${type_relations[@]}"
	type_relations=("${sorted_inventory[@]}")
fi
import_index=0
wildcard_index=0
type_index=0

for source_file in "${source_files[@]}"; do
	resolve_file "$source_file"
done

load_surface_path_violations
load_builder_path_violations
load_macro_path_violations

if [[ "${#violations[@]}" -gt 0 ]]; then
	sorted_violations=$(printf '%s\n' "${violations[@]}" | sort -t '|' -k1,1 -k2,2n -k3,3 -u)
	while IFS='|' read -r file line api; do
		printf '%s:%s — forbidden %s. Move SQL into a typed voom-store repository method.\n' \
			"$file" "$line" "$api" >&2
	done <<<"$sorted_violations"
	exit 1
fi

echo "control-plane SQL boundary: OK"
