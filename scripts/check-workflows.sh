#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tools="$repo_root/target/workflow-tools"
for tool in actionlint shellcheck zizmor; do
  if [[ ! -x "$tools/$tool" ]]; then
    echo "missing workflow analyzer $tool; run just setup-workflow-tools" >&2
    exit 1
  fi
done
cd "$repo_root"
if [[ $# -eq 0 ]]; then
  # Nullglob removes an unused extension without hiding an entirely empty set.
  shopt -s nullglob
  set -- .github/workflows/*.yml .github/workflows/*.yaml
fi
if [[ $# -eq 0 ]]; then
  echo 'no GitHub Actions workflows found' >&2
  exit 1
fi
"$tools/actionlint" -shellcheck "$tools/shellcheck" "$@"
"$tools/zizmor" --offline --strict-collection "$@"
