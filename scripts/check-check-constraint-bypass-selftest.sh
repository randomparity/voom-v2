#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
task_tmp=$(mktemp -d -t voom-check-constraint-bypass.XXXXXX)
trap 'rm -rf "$task_tmp"' EXIT
mkdir -p "$task_tmp/crates/voom-store/src" "$task_tmp/crates/example/src"

cat > "$task_tmp/crates/voom-store/src/test_support.rs" <<'EOF'
sqlx::query("PRAGMA ignore_check_constraints = ON");
EOF
VOOM_CHECK_CONSTRAINT_ROOT="$task_tmp" "$script_dir/check-check-constraint-bypass.sh"

cat > "$task_tmp/crates/example/src/test.rs" <<'EOF'
sqlx::query("PRAGMA ignore_check_constraints = ON");
EOF
if VOOM_CHECK_CONSTRAINT_ROOT="$task_tmp" "$script_dir/check-check-constraint-bypass.sh"; then
    echo "check-check-constraint-bypass-selftest: rejected source unexpectedly passed" >&2
    exit 1
fi

echo "check-check-constraint-bypass-selftest: OK"
