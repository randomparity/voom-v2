#!/usr/bin/env bash
set -euo pipefail

adr_dir=${VOOM_ADR_DIR:-docs/adr}
index_path=${VOOM_ADR_INDEX:-"$adr_dir/README.md"}

if [[ ! -d "$adr_dir" ]]; then
	echo "check-adr-index: ADR directory does not exist: $adr_dir" >&2
	exit 2
fi
if [[ ! -f "$index_path" ]]; then
	echo "check-adr-index: ADR index does not exist: $index_path" >&2
	exit 2
fi

errors=0
shopt -s nullglob
for adr_path in "$adr_dir"/[0-9][0-9][0-9][0-9]-*.md; do
	filename=$(basename "$adr_path")
	number=${filename%%-*}
	row_prefix="| [$number]($filename) |"
	match_count=$(
		awk -v prefix="$row_prefix" \
			'index($0, prefix) == 1 { count += 1 } END { print count + 0 }' \
			"$index_path"
	)
	if ((match_count == 0)); then
		echo "check-adr-index: $filename is missing from $index_path" >&2
		errors=$((errors + 1))
	elif ((match_count > 1)); then
		echo "check-adr-index: $filename appears $match_count times in $index_path" >&2
		errors=$((errors + 1))
	fi
done

while IFS= read -r filename; do
	[[ -z "$filename" ]] && continue
	if [[ ! -f "$adr_dir/$filename" ]]; then
		echo "check-adr-index: index link does not exist: $filename" >&2
		errors=$((errors + 1))
	fi
done < <(sed -nE 's/^\| \[[0-9]{4}\]\(([^)]+)\) \|.*/\1/p' "$index_path")

if ((errors > 0)); then
	echo "check-adr-index: $errors violation(s)." >&2
	exit 1
fi

echo "check-adr-index: OK"
