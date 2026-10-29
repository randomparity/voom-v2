#!/usr/bin/env bash
# Keep the #796 fingerprint experiment out of production identity evidence.
#
# The experiment's scripts and committed records must not name production hash fields,
# protocol result types, store writers, or the #788 harness's worker replay. A textual
# match is deliberately coarse: obfuscated references are left to review.
set -euo pipefail

root="${VOOM_FINGERPRINT_GUARD_ROOT:-.}"
pattern='content_hash|ObservedFileFacts|HashFileResult|file_versions|blake3|sqlite'
pattern+='|voom_store|INSERT INTO|/v1/operations|Worker|replay_file|HASH_FIELDS'
pattern+='|OBSERVED_FIELDS'

# bash 3.2: no mapfile, and an empty array under `set -u` is an error.
scope=()
count=0
for path in "$root"/scripts/media-evidence-fingerprint*.py \
	"$root"/docs/media-evidence-baseline/fingerprint/*.json; do
	if [[ -f "$path" ]]; then
		scope+=("$path")
		count=$((count + 1))
	fi
done

if [[ "$count" -eq 0 ]]; then
	echo "check-media-evidence-fingerprint-guard: no experiment files found under $root" >&2
	exit 1
fi

if grep -nE -- "$pattern" "${scope[@]}"; then
	echo "check-media-evidence-fingerprint-guard: experiment files reference production" \
		"identity evidence (matches above); keep fingerprints in experiment artifacts" >&2
	exit 1
fi

echo "check-media-evidence-fingerprint-guard: OK ($count files)"
