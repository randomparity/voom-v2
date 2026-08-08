#!/usr/bin/env bash
set -euo pipefail

root=${VOOM_CHECK_CONSTRAINT_ROOT:-.}
helper='crates/voom-store/src/test_support.rs'
matches=()
find_matches() {
    if command -v rg >/dev/null 2>&1; then
        rg -l 'ignore_check_constraints' "$root/crates" --glob '*.rs' || [[ $? -eq 1 ]]
    else
        grep -RIl --include='*.rs' 'ignore_check_constraints' "$root/crates" || [[ $? -eq 1 ]]
    fi
}
while IFS= read -r path; do
    matches+=("$path")
done < <(find_matches)

errors=0
if (( ${#matches[@]} > 0 )); then
    for path in "${matches[@]}"; do
        relative=${path#"$root/"}
        if [[ "$relative" != "$helper" ]]; then
            echo "check-check-constraint-bypass: $relative uses raw ignore_check_constraints" >&2
            echo "  Route the operation through voom_store::test_support::with_check_constraints_disabled." >&2
            errors=$((errors + 1))
        fi
    done
fi

if (( errors > 0 )); then
    echo "check-check-constraint-bypass: $errors violation(s)." >&2
    exit 1
fi

echo "check-check-constraint-bypass: OK"
