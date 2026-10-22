#!/usr/bin/env bash
# Sourced by acceptance entrypoints; descriptors 7/8/9 are reserved for this stack.
: "${repo_root:?set repository root before sourcing acceptance bootstrap}"
acceptance_pid=""
acceptance_ready=""

acceptance_stop() {
  if [[ -n "$acceptance_pid" ]]; then
    exec 9>&-
    local result=0
    wait "$acceptance_pid" || result=$?
    exec 7<&-
    acceptance_pid=""
    return "$result"
  fi
}

acceptance_start() {
  local directory=$1
  shift
  mkdir -p "$directory"
  mkfifo "$directory/bootstrap.stdin"
  exec 8<>"$directory/bootstrap.stdin"
  exec 7<"$directory/bootstrap.stdin"
  exec 9>"$directory/bootstrap.stdin"
  exec 8>&-
  acceptance_ready="$directory/bootstrap.ready.json"
  python3 "$repo_root/scripts/acceptance-bootstrap.py" serve --workdir "$directory" "$@" \
    <&7 7<&- 9>&- >"$acceptance_ready" 2>"$directory/bootstrap.stderr" &
  acceptance_pid=$!
  local deadline=$((SECONDS + 510))
  until jq -e '.status == "ready"' "$acceptance_ready" >/dev/null 2>&1; do
    if ! kill -0 "$acceptance_pid" 2>/dev/null || ((SECONDS >= deadline)); then
      kill -TERM "$acceptance_pid" 2>/dev/null || :
      acceptance_stop || :
      echo "acceptance bootstrap failed; inspect $directory/bootstrap.stderr" >&2
      return 1
    fi
    sleep 0.1
  done
  VOOM_LOCAL_NODE_ID=$(jq -er '.node_id' "$acceptance_ready")
  export VOOM_LOCAL_NODE_ID
}

acceptance_root() {
  jq -er --arg role "$1" --arg path "$2" \
    '.roots[] | select(.role == $role and .provider_locator == $path) | .root_id' "$acceptance_ready"
}
