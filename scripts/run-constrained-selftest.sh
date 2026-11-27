#!/usr/bin/env bash
# Self-test for run-constrained.sh.
#
# Exercises argument handling through --print-plan and device discovery with
# command doubles. Needs no systemd, cgroup delegation, or privileges.
#
# The platform gate itself is deliberately not asserted here: it depends on the
# host, and a test that only passes on one OS would be a false guard on the
# other.

set -uo pipefail

script="$(dirname "$0")/run-constrained.sh"
failures=0

# Run the script under a wall-clock bound where one is available. An option
# parser that stops consuming its input loops forever rather than exiting, and
# a self-test that hangs takes CI down with it instead of reporting a failure --
# which is exactly what a mutation of the `--cpus` arity check did while this
# was being written. macOS has no `timeout` in a default install, so the bound
# is best-effort: on that path the test still asserts, it just cannot bound.
if command -v timeout >/dev/null; then
	bounded() { timeout 10 "$@"; }
else
	bounded() { "$@"; }
fi

fail() {
	echo "FAIL: $1" >&2
	failures=$((failures + 1))
}

# Assert a `key<TAB>value` line is present in the plan.
expect_field() {
	local label=$1 field=$2 want=$3
	shift 3
	local plan got
	plan=$(bounded "$script" --print-plan "$@" 2>/dev/null)
	got=$(printf '%s\n' "$plan" | awk -F'\t' -v k="$field" '$1 == k {print $2}')
	[ "$got" = "$want" ] || fail "$label: expected $field=$want, got '${got:-<missing>}'"
}

expect_status() {
	local label=$1 want=$2
	shift 2
	bounded "$script" "$@" >/dev/null 2>&1
	local got=$?
	[ "$got" -eq "$want" ] || fail "$label: expected exit $want, got $got"
}

expect_stderr() {
	local label=$1 pattern=$2
	shift 2
	local err
	err=$(bounded "$script" "$@" 2>&1 >/dev/null)
	case $err in
	*"$pattern"*) ;;
	*) fail "$label: stderr did not mention '$pattern'; got: $err" ;;
	esac
}

# Defaults describe a GitHub-hosted ubuntu-latest runner.
expect_field "default cpus" cpus 0-3 -- cargo test
expect_field "default cpu count" cpu-count 4 -- cargo test
expect_field "default memory" memory 16G -- cargo test
expect_field "write cap off by default" write-bps unthrottled -- cargo test
expect_field "no competing load by default" load 0 -- cargo test

# Overrides land.
expect_field "cpus override" cpus 0-1 --cpus 0-1 -- cargo test
expect_field "memory override" memory 2G --memory 2G -- cargo test
expect_field "write cap override" write-bps 40M --write-bps 40M -- cargo test
expect_field "load override" load 3 --load 3 -- cargo test

# cpu-count counts individual cpus, not list entries. A --load of N spawns
# N loops per cpu, so a miscount silently changes how much contention a
# reproduction actually applied.
expect_field "range is counted per cpu" cpu-count 4 --cpus 0-3 -- cargo test
expect_field "comma list is counted per cpu" cpu-count 2 --cpus 0,5 -- cargo test
expect_field "mixed list is counted per cpu" cpu-count 3 --cpus 0-1,4 -- cargo test
expect_field "single cpu" cpu-count 1 --cpus 7 -- cargo test

# The command survives the -- separator intact, including its own flags.
expect_field "command passes through" command "cargo test -- --test-threads=1" \
	-- cargo test -- --test-threads=1

# Rejections. An inverted range must not reach the plan: the expansion runs
# inside a command substitution, where an exit would end only the subshell and
# leave a rejected input reported as success.
expect_status "inverted range is rejected" 2 --cpus 3-1 --print-plan -- echo hi
expect_stderr "inverted range says why" "inverted cpu range" --cpus 3-1 --print-plan -- echo hi
expect_status "non-numeric load is rejected" 2 --load x --print-plan -- echo hi
expect_status "negative load is rejected" 2 --load -1 --print-plan -- echo hi
expect_status "non-numeric cpus are rejected" 2 --cpus all --print-plan -- echo hi
expect_status "unknown option is rejected" 2 --bogus --print-plan -- echo hi
expect_status "missing command is rejected" 2 --print-plan --
expect_status "missing command without -- is rejected" 2 --print-plan
expect_status "option without its value is rejected" 2 --cpus
expect_stderr "missing command says how" "separate it with --" --print-plan --

# --help is not an error.
expect_status "help exits 0" 0 --help

# Exercise the execution path without depending on Linux, a block device, or
# cgroup delegation. Only the platform/device probes are doubled; systemd-run
# captures arguments instead of launching anything. BASH_ENV confines these
# functions to the child Bash, including the one block-file predicate.
fixtures=$(mktemp -d)
trap 'rm -rf "$fixtures"' EXIT
cat > "$fixtures/environment" <<'MOCKS'
uname() { echo Linux; }
stat() { echo cgroup2fs; }
cat() { echo 'memory io'; }
findmnt() { printf '%s\n' "$MOCK_SOURCE"; }
lsblk() {
	# A whole disk can have children: omitting -d must not select a child's
	# PKNAME as if it were the source's own parent.
	case $* in
	'-dno PKNAME '*) printf '%s\n' "$MOCK_PARENT" ;;
	*) printf '%s\n' "$MOCK_PARENT" "$MOCK_CHILD_PARENT" ;;
	esac
}
[() {
	if [[ ${1:-} == -b ]]; then
		[[ $2 == /dev/fixture-disk || $2 == /dev/fixture-override ]]
	else
		builtin [ "$@"
	fi
}
MOCKS
mkdir -p "$fixtures/bin"
cat > "$fixtures/bin/systemd-run" <<'MOCK_RUN'
#!/usr/bin/env bash
printf '%s\n' "$@"
MOCK_RUN
cp "$fixtures/bin/systemd-run" "$fixtures/bin/taskset"
chmod +x "$fixtures/bin/systemd-run" "$fixtures/bin/taskset"

device_case() {
	local label=$1 source=$2 parent=$3 want_status=$4 want_device=$5
	shift 5
	local output status
	output=$(PATH="$fixtures/bin:$PATH" BASH_ENV="$fixtures/environment" MOCK_SOURCE="$source" \
		MOCK_PARENT="$parent" MOCK_CHILD_PARENT=wrong-child \
		bounded "$script" --write-bps 40M "$@" -- echo constrained 2>"$fixtures/error")
	status=$?
	[ "$status" -eq "$want_status" ] || fail "$label: expected exit $want_status, got $status"
	if [ "$want_status" -eq 0 ]; then
		printf '%s\n' "$output" | grep -Fx "IOWriteBandwidthMax=$want_device 40M" >/dev/null ||
			fail "$label: write cap did not reach the selected device"
		printf '%s\n' "$output" | grep -Fx 'MemoryMax=16G' >/dev/null ||
			fail "$label: memory cap lost"
		printf '%s\n' "$output" | grep -Fx '0-3' >/dev/null ||
			fail "$label: cpu affinity lost"
		printf '%s\n' "$output" | grep -Fx 'constrained' >/dev/null ||
			fail "$label: command lost"
	else
		[ -z "$output" ] || fail "$label: rejected device reached systemd-run"
	fi
}

device_case 'whole disk without parent' /dev/fixture-disk '' 0 /dev/fixture-disk
device_case 'partition selects parent' /dev/fixture-part fixture-disk 0 /dev/fixture-disk
device_case 'subvolume selects parent' '/dev/fixture-part[/home]' fixture-disk 0 /dev/fixture-disk
device_case 'explicit device skips discovery' '' '' 0 /dev/fixture-override --device /dev/fixture-override
device_case 'unresolved source is rejected' '' '' 2 ''
device_case 'non-block source is rejected' overlay '' 2 ''
device_case 'non-block override is rejected' '' '' 2 '' --device /dev/missing

if [ "$failures" -gt 0 ]; then
	echo "run-constrained-selftest: $failures failure(s)" >&2
	exit 1
fi
echo "run-constrained-selftest: all checks passed"
