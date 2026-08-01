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

collect_function() {
	local qualifier="$1"
	local api="$2"
	shift 2
	collect_pattern "$api" "${qualifier}(\$\$\$ARGS)" "$@"
	collect_pattern "$api" "${qualifier}::<\$\$\$TYPES>(\$\$\$ARGS)" "$@"
}

for function in query query_as query_scalar raw_sql; do
	collect_function "sqlx::$function" "sqlx::$function" "${source_files[@]}"
done

for macro_name in query query_as query_scalar query_file query_file_as query_file_scalar; do
	collect_pattern "sqlx::$macro_name!" "sqlx::$macro_name!(\$\$\$ARGS)" "${source_files[@]}"
done

collect_pattern "sqlx::QueryBuilder::new" "sqlx::QueryBuilder::new(\$\$\$ARGS)" \
	"${source_files[@]}"
collect_pattern "sqlx::QueryBuilder::new" \
	"sqlx::QueryBuilder::<\$\$\$TYPES>::new(\$\$\$ARGS)" \
	"${source_files[@]}"

alias_rule='
id: sqlx-import-alias
language: rust
severity: error
rule:
  kind: use_as_clause
  regex: "^(sqlx::)?(query|query_as|query_scalar|raw_sql|QueryBuilder) as [A-Za-z_][A-Za-z0-9_]*$"
  inside:
    kind: use_declaration
    regex: "(?s)^use\\s+sqlx\\s*::"
    stopBy: end
'

alias_output=""
alias_status=0
set +e
alias_output=$(ast-grep scan --inline-rules "$alias_rule" --json=stream \
	"${source_files[@]}" 2>/dev/null)
alias_status=$?
set -e
if [[ "$alias_status" -gt 1 ]]; then
	echo "check-control-plane-sql-boundary: ast-grep failed while finding SQLx aliases" >&2
	exit 2
fi

while IFS= read -r alias_match; do
	[[ -z "$alias_match" ]] && continue
	if [[ "$alias_match" =~ \"file\":\"([^\"]*)\" ]]; then
		alias_file=${BASH_REMATCH[1]}
	else
		alias_file=""
	fi
	if [[ "$alias_match" =~ \"text\":\"([^\"]*)\" ]]; then
		alias_text=${BASH_REMATCH[1]}
	else
		alias_text=""
	fi
	original=${alias_text%% as *}
	original=${original##*::}
	alias=${alias_text##* as }
	if [[ -z "$alias_file" || -z "$original" || -z "$alias" ]]; then
		echo "check-control-plane-sql-boundary: could not parse SQLx alias match: $alias_match" >&2
		exit 2
	fi
	if [[ "$original" == "QueryBuilder" ]]; then
		collect_pattern "sqlx::QueryBuilder::new" "${alias}::new(\$\$\$ARGS)" "$alias_file"
		collect_pattern "sqlx::QueryBuilder::new" \
			"${alias}::<\$\$\$TYPES>::new(\$\$\$ARGS)" "$alias_file"
	else
		collect_function "$alias" "sqlx::$original" "$alias_file"
		collect_pattern "sqlx::$original!" "${alias}!(\$\$\$ARGS)" "$alias_file"
	fi
done <<<"$alias_output"

if [[ "${#violations[@]}" -gt 0 ]]; then
	sorted_violations=$(printf '%s\n' "${violations[@]}" | sort -t '|' -k1,1 -k2,2n -k3,3 -u)
	while IFS='|' read -r file line api; do
		printf '%s:%s — forbidden %s. Move SQL into a typed voom-store repository method.\n' \
			"$file" "$line" "$api" >&2
	done <<<"$sorted_violations"
	exit 1
fi

echo "control-plane SQL boundary: OK"
