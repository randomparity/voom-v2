#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workdir="$(mktemp -d -t voom-chaos-script-test.XXXXXX)"
fake_bin="$workdir/bin"
fake_voom="$workdir/voom"
chaos_workdir="$workdir/chaos-work"
mkdir -p "$fake_bin"
trap 'rm -rf "$workdir"' EXIT

cat >"$fake_bin/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == *"submodule status third_party/chaos-librarian"* ]]; then
  echo " 057a4033a3a9ae14fef664ab82f2c31e1a223544 third_party/chaos-librarian (heads/main)"
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
  echo '{"ready_for":{"materialize_media_mutations":true}}'
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
  sleep 0.25
  mkdir -p "$out/movies-hd"
  printf 'media' >"$out/movies-hd/movie.mkv"
  sleep 0.25
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
  scan\ --path\ *)
    library="${args[$((${#args[@]} - 1))]}"
    if ! find "$library" -type f -name '*.mkv' -print -quit | grep -q .; then
      echo '{"status":"error","error":{"code":"MISSING_LIBRARY"}}'
      exit 2
    fi
    echo '{"status":"ok","data":{"files":[{"status":"scanned","file_version_id":11,"media_snapshot_id":22}]}}'
    ;;
  policy\ input\ create-from-scan\ *)
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

chmod +x "$fake_bin/git" "$fake_bin/cargo" "$fake_bin/uv" "$fake_voom"

PATH="$fake_bin:$PATH" \
VOOM_BIN="$fake_voom" \
CHAOS_WORKDIR="$chaos_workdir" \
CHAOS_DURATION="1s" \
CHAOS_SPEED="1x" \
CHAOS_CHECKPOINT_INTERVAL="0.1s" \
CHAOS_EXECUTE_POLICY=1 \
CHAOS_POLICY_VERSION_ID=7 \
CHAOS_PRESERVE_OUTPUT=0 \
CHAOS_CLEANUP=0 \
  "$repo_root/scripts/chaos-e2e-local.sh"

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
