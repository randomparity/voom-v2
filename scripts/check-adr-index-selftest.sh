#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
task_tmp=$(mktemp -d /tmp/voom-adr-index-selftest.XXXXXX)

cleanup() {
	rm -f "$task_tmp"/*
	rmdir "$task_tmp"
}
trap cleanup EXIT

printf '# First\n' >"$task_tmp/0001-first.md"
printf '# Second\n' >"$task_tmp/0002-second.md"
printf '| ADR | Decision |\n|---|---|\n| [0001](0001-first.md) | First |\n' \
	>"$task_tmp/README.md"

if VOOM_ADR_DIR="$task_tmp" VOOM_ADR_INDEX="$task_tmp/README.md" \
	"$script_dir/check-adr-index.sh" >/dev/null 2>&1; then
	echo "check-adr-index-selftest: missing ADR row unexpectedly passed" >&2
	exit 1
fi

printf '| [0002](0002-second.md) | Second |\n' >>"$task_tmp/README.md"
VOOM_ADR_DIR="$task_tmp" VOOM_ADR_INDEX="$task_tmp/README.md" \
	"$script_dir/check-adr-index.sh" >/dev/null

printf '| [0003](0003-missing.md) | Missing |\n' >>"$task_tmp/README.md"
if VOOM_ADR_DIR="$task_tmp" VOOM_ADR_INDEX="$task_tmp/README.md" \
	"$script_dir/check-adr-index.sh" >/dev/null 2>&1; then
	echo "check-adr-index-selftest: broken ADR link unexpectedly passed" >&2
	exit 1
fi

echo "check-adr-index-selftest: OK"
