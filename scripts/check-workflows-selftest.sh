#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tools="$repo_root/target/workflow-tools"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
cat > "$fixture/safe.yml" <<'YAML'
name: control
on: workflow_dispatch
permissions:
  contents: read
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - run: echo safe
YAML
"$repo_root/scripts/check-workflows.sh" "$fixture/safe.yml"
expect_failure() {
  local diagnostic=$1
  shift
  if "$@" > "$fixture/output" 2>&1; then
    echo "expected rejection: $diagnostic" >&2
    exit 1
  fi
  if ! grep -qF "$diagnostic" "$fixture/output"; then
    cat "$fixture/output" >&2
    echo "missing diagnostic: $diagnostic" >&2
    exit 1
  fi
}
sed '/runs-on:/a\
    needs: absent
' "$fixture/safe.yml" > "$fixture/schema.yml"
expect_failure 'job "absent"' "$repo_root/scripts/check-workflows.sh" "$fixture/schema.yml"
# shellcheck disable=SC2016 # The fixture needs a literal, deliberately unquoted variable.
sed 's/echo safe/echo $GITHUB_REPOSITORY/' "$fixture/safe.yml" > "$fixture/shell.yml"
expect_failure SC2086 "$repo_root/scripts/check-workflows.sh" "$fixture/shell.yml"
sed 's/echo safe/echo "${{ github.event.issue.title }}"/' \
  "$fixture/safe.yml" > "$fixture/injection.yml"
expect_failure 'potentially untrusted' "$repo_root/scripts/check-workflows.sh" "$fixture/injection.yml"
expect_failure template-injection "$tools/zizmor" --offline "$fixture/injection.yml"
cat > "$fixture/cache.yml" <<'YAML'
name: release control
on:
  push:
    tags: ['v*']
permissions:
  contents: read
jobs:
  release:
    runs-on: ubuntu-latest
    steps:
      - uses: Swatinem/rust-cache@258712b0b7b1ddf8bddc9fc3b0faca682b2736c3
YAML
expect_failure cache-poisoning "$repo_root/scripts/check-workflows.sh" "$fixture/cache.yml"
# Copy the gate to an empty checkout to prove missing tools never skip analysis.
mkdir "$fixture/scripts"
cp "$repo_root/scripts/check-workflows.sh" "$fixture/scripts/"
expect_failure 'just setup-workflow-tools' "$fixture/scripts/check-workflows.sh" "$fixture/safe.yml"
# Corrupt download content must fail before extraction or replacing any binary.
cp "$repo_root/scripts/setup-workflow-tools.sh" "$fixture/scripts/"
mkdir "$fixture/bin"
cat > "$fixture/bin/curl" <<'SH'
#!/usr/bin/env bash
while [[ "$1" != -o ]]; do shift; done
printf corrupt > "$2"
SH
chmod +x "$fixture/bin/curl"
expect_failure 'SHA256 mismatch' env PATH="$fixture/bin:$PATH" \
  "$fixture/scripts/setup-workflow-tools.sh"
test ! -e "$fixture/target/workflow-tools/actionlint"
"$tools/shellcheck" "$repo_root/scripts/check-workflows.sh" \
  "$repo_root/scripts/check-workflows-selftest.sh" "$repo_root/scripts/setup-workflow-tools.sh"
echo 'workflow guard self-test: safe control and 7 rejection cases passed'
