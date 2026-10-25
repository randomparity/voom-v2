#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workdir="$(mktemp -d -t voom-chaos-script-test.XXXXXX)"
fake_bin="$workdir/bin"
fake_voom="$workdir/voom"
real_python=$(command -v python3)
export CHAOS_TEST_REAL_PYTHON="$real_python"
chaos_workdir="$workdir/chaos-work"
mkdir -p "$fake_bin"
trap 'rm -rf "$workdir"' EXIT

cat >"$fake_bin/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == *"submodule status third_party/chaos-librarian"* ]]; then
  echo " 3b26eb9625babca61feaf1f56cb7ed8d36f1bd1a third_party/chaos-librarian (heads/main)"
elif [[ "$*" == *"status --short --untracked-files=no"* ]]; then
  :
else
  echo "unexpected git invocation: $*" >&2
  exit 1
fi
SH

cat >"$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  build) exit 0 ;;
  run) echo '{"status":"ok"}' ;;
  *)
    echo "unexpected cargo invocation: $*" >&2
    exit 1
    ;;
esac
SH

cat >"$fake_bin/uv" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == "sync" ]]; then
  exit 0
fi
if [[ "$1" == "run" && "$2" == "chaos-librarian" && "$3" == "capabilities" ]]; then
  echo '{"ready_for":{"materialize_static":true,"materialize_filesystem_mutations":true,"materialize_media_mutations":true}}'
  exit 0
fi
if [[ "$1" == "run" && "$2" == "python" && "$3" == */fixed_seed_corpus.py && "$4" == summary ]]; then
  jq -nc --argjson applied "${FAKE_APPLIED_EVENTS:-11}" \
    '{applied_events:$applied,timeline_events:11,timeline_complete:($applied == 11)}'
  exit 0
fi
if [[ "$1" == "run" && "$2" == "chaos-librarian" && "$3" == "generate" ]]; then
  [[ " $* " == *" --seed 248 "* && " $* " == *" --lane adversarial-media "* ]]
  while [[ "$1" != "--out" ]]; do shift; done
  printf 'seed: 248\n' > "$2"
  echo '{"seed":248,"lane":"adversarial-media"}'
  exit 0
fi
if [[ "$1" == "run" && "$2" == "chaos-librarian" && "$3" == "run" ]]; then
  out=""
  while [[ "$#" -gt 0 ]]; do
    if [[ "$1" == "--out" ]]; then
      out="$2"
      break
    fi
    shift
  done
  if [[ -z "$out" ]]; then
    echo "missing --out" >&2
    exit 1
  fi
  mkdir -p "$out"
  printf '%s\n' "$$" >"$out/producer-pid"
  sleep 0.25
  mkdir -p "$out/library/movies-hd"
  printf 'media' >"$out/library/movies-hd/movie.mkv"
  sleep 1
  echo '{"ok":true}'
  exit 0
fi
echo "unexpected uv invocation: $*" >&2
exit 1
SH

cat >"$fake_voom" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
  if [[ "${args[$i]}" == "--database-url" ]]; then
    unset "args[$i]" "args[$((i + 1))]"
    break
  fi
done
args=("${args[@]}")
case "${args[*]}" in
  scan\ --root\ 3)
    test "$VOOM_LOCAL_NODE_ID" = 7
    echo '{"status":"ok","data":{"status":"succeeded","observation_count":1}}'
    ;;
  policy\ input\ create-from-scan\ *)
    [[ " ${args[*]} " == *" --root 3 "* ]]
    echo '{"status":"ok","data":{"input_set":{"input_set_id":101}}}'
    ;;
  compliance\ report\ *)
    echo '{"status":"ok","data":{"report":{"summary":{"status":"compliant"}}}}'
    ;;
  *)
    echo "unexpected voom invocation: ${args[*]}" >&2
    exit 1
    ;;
esac
SH

# Fake only the external bootstrap process; run the real Bash lifecycle adapter.
cat >"$fake_bin/python3" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" != */scripts/acceptance-bootstrap.py ]]; then
  exec "$CHAOS_TEST_REAL_PYTHON" "$@"
fi
[[ "$2" == serve ]]
if [[ "${FAKE_BOOTSTRAP_WAIT_FOR_PRODUCER_EXIT:-0}" == 1 ]]; then
  while kill -0 "$(cat "$CHAOS_WORKDIR/run/producer-pid")" 2>/dev/null; do
    sleep 0.01
  done
fi
jq -nc --arg path "$CHAOS_WORKDIR/run/library" \
  '{status:"ready",node_id:7,roots:[{role:"input",provider_locator:$path,root_id:3}]}'
cat >/dev/null
printf 'stopped\n' >"$CHAOS_WORKDIR/bootstrap-stopped"
exit "${FAKE_BOOTSTRAP_EXIT_CODE:-0}"
SH

chmod +x "$fake_bin/git" "$fake_bin/cargo" "$fake_bin/uv" "$fake_bin/python3" "$fake_voom"

run_case() {
PATH="$fake_bin:$PATH" \
VOOM_BIN="${FAKE_VOOM_OVERRIDE-$fake_voom}" \
CHAOS_WORKDIR="$1" \
CHAOS_DURATION="1s" \
CHAOS_SPEED="1x" \
CHAOS_CHECKPOINT_INTERVAL="0.1s" \
CHAOS_EXECUTE_POLICY=1 \
CHAOS_POLICY_VERSION_ID=7 \
CHAOS_PRESERVE_OUTPUT=0 \
CHAOS_CLEANUP=0 \
  "$repo_root/scripts/chaos-e2e-local.sh"

}
run_case "$chaos_workdir"

test -s "$chaos_workdir/summary.jsonl" || {
  echo "expected local chaos script to record at least one checkpoint" >&2
  exit 1
}

jq -e '
  select(.policy_status == "reported")
  | select(.policy_input_set_id == "101")
  | select(.policy_report_summary_status == "compliant")
  | select(.policy_ticket_count == 0)
' "$chaos_workdir/summary.jsonl" >/dev/null

test -s "$chaos_workdir/bootstrap-stopped"
jq -e '.timeline_complete == true and .applied_events == 11' "$chaos_workdir/timeline-summary.json" >/dev/null
FAKE_APPLIED_EVENTS=1 run_case "$workdir/prefix"
jq -e '.timeline_complete == false and .applied_events == 1 and .timeline_events == 11' \
  "$workdir/prefix/timeline-summary.json" >/dev/null

if FAKE_BOOTSTRAP_EXIT_CODE=2 run_case "$workdir/failed-shutdown"; then
  echo "Chaos falsely succeeded after bootstrap shutdown failed" >&2
  exit 1
fi

if FAKE_BOOTSTRAP_WAIT_FOR_PRODUCER_EXIT=1 run_case "$workdir/no-checkpoint"; then
  echo "Chaos falsely succeeded without a scan checkpoint" >&2
  exit 1
fi

mkdir -p "$workdir/target/debug"
cp "$fake_voom" "$workdir/target/debug/voom"
relative_target=$(python3 -c 'import os, sys; print(os.path.relpath(sys.argv[1], sys.argv[2]))' \
  "$workdir/target" "$repo_root")
FAKE_VOOM_OVERRIDE='' CARGO_TARGET_DIR="$relative_target" run_case "$workdir/relative-target"
test -s "$workdir/relative-target/summary.jsonl"

CHAOS_SEED=248 CHAOS_LANE=adversarial-media run_case "$workdir/explicit-lane"
jq -e '.seed == 248 and .lane == "adversarial-media"' "$workdir/explicit-lane/generation.json"
if CHAOS_SEED=248 run_case "$workdir/missing-lane"; then
  echo "seed without lane falsely succeeded" >&2
  exit 1
fi
