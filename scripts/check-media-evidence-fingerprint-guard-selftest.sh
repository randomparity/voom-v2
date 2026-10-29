#!/usr/bin/env bash
# Prove the #796 fingerprint guard fails on each forbidden token and on an empty scope.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
guard="$script_dir/check-media-evidence-fingerprint-guard.sh"
task_tmp=$(mktemp -d /tmp/voom-fingerprint-guard-selftest.XXXXXX)

cleanup() {
	rm -rf "$task_tmp"
}
trap cleanup EXIT

expect_failure() {
	if VOOM_FINGERPRINT_GUARD_ROOT="$task_tmp" "$guard" >/dev/null 2>&1; then
		echo "check-media-evidence-fingerprint-guard-selftest: $1 unexpectedly passed" >&2
		exit 1
	fi
}

mkdir -p "$task_tmp/scripts" "$task_tmp/docs/media-evidence-baseline/fingerprint"
expect_failure "empty scope"

clean="$task_tmp/scripts/media-evidence-fingerprint.py"
printf 'digest = hashlib.blake2b()\n' >"$clean"
printf '{"schema": "media-evidence-fingerprint-run/1"}\n' \
	>"$task_tmp/docs/media-evidence-baseline/fingerprint/run.json"
VOOM_FINGERPRINT_GUARD_ROOT="$task_tmp" "$guard" >/dev/null

tokens=(content_hash ObservedFileFacts HashFileResult file_versions blake3 sqlite
	voom_store 'INSERT INTO' /v1/operations Worker replay_file HASH_FIELDS OBSERVED_FIELDS)
for token in "${tokens[@]}"; do
	for target in "$clean" "$task_tmp/scripts/media-evidence-fingerprint-selftest.py" \
		"$task_tmp/docs/media-evidence-baseline/fingerprint/run.json"; do
		printf 'x = "%s"\n' "$token" >"$target.planted"
		mv "$target.planted" "$target"
		expect_failure "token '$token' in ${target#"$task_tmp"/}"
		printf 'digest = hashlib.blake2b()\n' >"$target"
	done
done

VOOM_FINGERPRINT_GUARD_ROOT="$task_tmp" "$guard" >/dev/null
echo "check-media-evidence-fingerprint-guard-selftest: OK"
