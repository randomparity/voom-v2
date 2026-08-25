#!/usr/bin/env bash
# Run a command under GitHub-hosted-runner-like resource limits.
#
# A workstation is a poor stand-in for a 4 vCPU / 16G runner, and some races
# only appear under memory pressure or slow storage. This wraps a command in a
# cgroup v2 scope so those two can be reached locally.
#
# Deliberately narrow. Ablation on issue #546 showed constraint knobs did not
# separate the cells -- that race reproduces unconstrained on idle hardware at
# roughly 1 run in 8 -- so `just test-repeat` is the first thing to reach for
# and this is for what repetition cannot reach.
#
# CPU count is set with taskset affinity rather than CPUQuota on purpose:
# Rust's available_parallelism() reads cgroup cpu.max as well as
# sched_getaffinity, so a quota would silently lower the thread count cargo and
# tokio choose instead of giving the command 4 busy cores. To emulate a slower
# shared vCPU, --load pins competing busy loops to the same cores, which leaves
# the reported parallelism alone.
#
# Linux + systemd + cgroup v2 only. See docs/adr/0079-deterministic-test-temp-root.md.

set -uo pipefail

CPUS=0-3
MEMORY=16G
WRITE_BPS=
LOAD=0
DEVICE=
PRINT_PLAN=0

usage() {
	cat <<'USAGE'
usage: run-constrained.sh [options] -- COMMAND [ARGS...]

  --cpus LIST       taskset cpu list          (default 0-3, i.e. 4 vCPU)
  --memory SIZE     cgroup MemoryMax          (default 16G)
  --write-bps RATE  cgroup write cap, e.g. 40M (default: unthrottled)
  --device PATH     block device for --write-bps (default: the one backing $PWD)
  --load N          competing busy loops per cpu (default 0; 1 is roughly half speed)
  --print-plan      resolve and print the configuration, run nothing
USAGE
}

die() {
	echo "run-constrained: $1" >&2
	exit "${2:-2}"
}

while [ $# -gt 0 ]; do
	case $1 in
	--cpus)
		[ $# -ge 2 ] || die "--cpus needs a value"
		CPUS=$2
		shift 2
		;;
	--memory)
		[ $# -ge 2 ] || die "--memory needs a value"
		MEMORY=$2
		shift 2
		;;
	--write-bps)
		[ $# -ge 2 ] || die "--write-bps needs a value"
		WRITE_BPS=$2
		shift 2
		;;
	--device)
		[ $# -ge 2 ] || die "--device needs a value"
		DEVICE=$2
		shift 2
		;;
	--load)
		[ $# -ge 2 ] || die "--load needs a value"
		LOAD=$2
		shift 2
		;;
	--print-plan)
		PRINT_PLAN=1
		shift
		;;
	-h | --help)
		usage
		exit 0
		;;
	--)
		shift
		break
		;;
	*) die "unknown option: $1" ;;
	esac
done

[[ $LOAD =~ ^[0-9]+$ ]] || die "--load must be a non-negative integer, got: $LOAD"
[[ $CPUS =~ ^[0-9]+([,-][0-9]+)*$ ]] || die "--cpus must be a cpu list like 0-3 or 0,2, got: $CPUS"
[ $# -gt 0 ] || die "no command given; separate it with --"

# Expand a taskset list ("0-3", "0,2", "0-1,4") into individual cpu ids, so
# --load can pin its busy loops one cpu at a time.
#
# Never call `die` from here. Both call sites are command substitutions, where
# an `exit` ends only the subshell: the error text reaches the terminal, the
# caller carries on, and the script reports success on a rejected input. Ranges
# are validated up front instead, before any substitution runs.
expand_cpus() {
	local part start end
	for part in ${1//,/ }; do
		if [[ $part == *-* ]]; then
			start=${part%%-*}
			end=${part##*-}
			seq "$start" "$end"
		else
			printf '%s\n' "$part"
		fi
	done
}

for range in ${CPUS//,/ }; do
	[[ $range == *-* ]] || continue
	[ "${range%%-*}" -le "${range##*-}" ] || die "inverted cpu range: $range"
done

if [ "$PRINT_PLAN" -eq 1 ]; then
	# One key per line so the selftest can assert on fields without a shell parser.
	printf 'cpus\t%s\n' "$CPUS"
	printf 'cpu-count\t%s\n' "$(expand_cpus "$CPUS" | wc -l | tr -d ' ')"
	printf 'memory\t%s\n' "$MEMORY"
	printf 'write-bps\t%s\n' "${WRITE_BPS:-unthrottled}"
	printf 'load\t%s\n' "$LOAD"
	printf 'command\t%s\n' "$*"
	exit 0
fi

[ "$(uname -s)" = "Linux" ] || die "resource limits need Linux cgroup v2; on this host use \`just test-repeat\` or \`just test-serial\` instead" 3
command -v systemd-run >/dev/null || die "systemd-run not found; needed for a --user cgroup scope" 3
command -v taskset >/dev/null || die "taskset not found (util-linux); needed to pin cpus" 3
[ "$(stat -fc %T /sys/fs/cgroup 2>/dev/null)" = "cgroup2fs" ] || die "/sys/fs/cgroup is not cgroup v2" 3

delegated=$(cat "/sys/fs/cgroup/user.slice/user-$(id -u).slice/cgroup.controllers" 2>/dev/null || true)
case " $delegated " in
*" memory "*) ;;
*) die "the memory controller is not delegated to this user slice (have: ${delegated:-none})" 3 ;;
esac

hogs=()
cleanup() {
	[ ${#hogs[@]} -gt 0 ] || return 0
	kill "${hogs[@]}" 2>/dev/null
	wait "${hogs[@]}" 2>/dev/null
	return 0
}
trap cleanup EXIT INT TERM

if [ "$LOAD" -gt 0 ]; then
	while read -r cpu; do
		for _ in $(seq 1 "$LOAD"); do
			taskset -c "$cpu" sh -c 'while :; do :; done' &
			hogs+=($!)
		done
	done < <(expand_cpus "$CPUS")
	echo "run-constrained: ${#hogs[@]} competing loops pinned to cpus $CPUS" >&2
fi

properties=(-p "MemoryMax=$MEMORY")
if [ -n "$WRITE_BPS" ]; then
	if [ -z "$DEVICE" ]; then
		# io.max keys on the whole block device, not the partition, so strip a
		# trailing partition suffix from whatever backs the working directory.
		source_device=$(findmnt -no SOURCE --target . 2>/dev/null | sed 's/\[.*//')
		DEVICE=$(lsblk -no PKNAME "$source_device" 2>/dev/null | head -n1)
		[ -n "$DEVICE" ] || die "could not resolve a block device for $PWD; pass --device"
		DEVICE=/dev/$DEVICE
	fi
	[ -b "$DEVICE" ] || die "not a block device: $DEVICE"
	properties+=(-p "IOWriteBandwidthMax=$DEVICE $WRITE_BPS")
fi

echo "run-constrained: cpus=$CPUS memory=$MEMORY write-bps=${WRITE_BPS:-unthrottled} load=$LOAD" >&2
exec systemd-run --user --scope --quiet --collect "${properties[@]}" -- taskset -c "$CPUS" "$@"
