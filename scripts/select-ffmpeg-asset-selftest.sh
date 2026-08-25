#!/usr/bin/env bash
# Self-test for scripts/select-ffmpeg-asset.sh.
#
# Each case pins one rule of the selection. Break a rule and exactly one case
# should redden -- see the mutation table in the issue-536 implementation plan.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
select_script="$script_dir/select-ffmpeg-asset.sh"
task_tmp=$(mktemp -d /tmp/voom-select-ffmpeg-selftest.XXXXXX)

cleanup() {
	rm -f "$task_tmp"/*
	rmdir "$task_tmp"
}
trap cleanup EXIT

failures=0

# Assert stdout and exit status.
#   expect_ok <case> <floor> <expected-stdout> < <input>
expect_ok() {
	local case_name=$1 floor=$2 expected=$3 actual status
	set +e
	actual=$("$select_script" "$floor" 2>"$task_tmp/err")
	status=$?
	set -e
	if ((status != 0)); then
		echo "select-ffmpeg-asset-selftest: $case_name: expected exit 0, got $status" >&2
		cat "$task_tmp/err" >&2
		failures=$((failures + 1))
		return
	fi
	if [[ $actual != "$expected" ]]; then
		echo "select-ffmpeg-asset-selftest: $case_name: expected '$expected', got '$actual'" >&2
		failures=$((failures + 1))
	fi
}

# Assert exit status only.
#   expect_status <case> <expected-status> <args...> < <input>
expect_status() {
	local case_name=$1 expected=$2
	shift 2
	local status
	set +e
	"$select_script" "$@" >"$task_tmp/out" 2>"$task_tmp/err"
	status=$?
	set -e
	if ((status != expected)); then
		echo "select-ffmpeg-asset-selftest: $case_name: expected exit $expected, got $status" >&2
		cat "$task_tmp/err" >&2
		failures=$((failures + 1))
	fi
}

# Assert exit status AND a substring of stderr. Without this, swapping the exit-1
# and exit-3 messages would stay green -- defeating the reason the two are separate.
#   expect_stderr <case> <expected-status> <substring> <args...> < <input>
expect_stderr() {
	local case_name=$1 expected=$2 needle=$3
	shift 3
	local status
	set +e
	"$select_script" "$@" >"$task_tmp/out" 2>"$task_tmp/err"
	status=$?
	set -e
	if ((status != expected)); then
		echo "select-ffmpeg-asset-selftest: $case_name: expected exit $expected, got $status" >&2
		cat "$task_tmp/err" >&2
		failures=$((failures + 1))
		return
	fi
	if ! grep -qF -- "$needle" "$task_tmp/err"; then
		echo "select-ffmpeg-asset-selftest: $case_name: stderr lacks '$needle'" >&2
		cat "$task_tmp/err" >&2
		failures=$((failures + 1))
	fi
}

# BtbN's complete `latest` catalogue on 2026-08-25, as returned by
#   gh api repos/BtbN/FFmpeg-Builds/releases/tags/latest --jq '.assets[].name'
# Kept whole on purpose: the linuxarm64, win64, winarm64 and lgpl entries are what
# prove the selection discriminates on platform and licence, not just on version.
catalogue() {
	cat <<-'ASSETS'
	checksums.sha256
	ffmpeg-master-latest-linux64-gpl-shared.tar.xz
	ffmpeg-master-latest-linux64-gpl.tar.xz
	ffmpeg-master-latest-linux64-lgpl-shared.tar.xz
	ffmpeg-master-latest-linux64-lgpl.tar.xz
	ffmpeg-master-latest-linuxarm64-gpl-shared.tar.xz
	ffmpeg-master-latest-linuxarm64-gpl.tar.xz
	ffmpeg-master-latest-linuxarm64-lgpl-shared.tar.xz
	ffmpeg-master-latest-linuxarm64-lgpl.tar.xz
	ffmpeg-master-latest-win64-gpl-shared.zip
	ffmpeg-master-latest-win64-gpl.zip
	ffmpeg-master-latest-win64-lgpl-shared.zip
	ffmpeg-master-latest-win64-lgpl.zip
	ffmpeg-master-latest-winarm64-gpl-shared.zip
	ffmpeg-master-latest-winarm64-gpl.zip
	ffmpeg-master-latest-winarm64-lgpl-shared.zip
	ffmpeg-master-latest-winarm64-lgpl.zip
	ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz
	ffmpeg-n8.1-latest-linux64-gpl-shared-8.1.tar.xz
	ffmpeg-n8.1-latest-linux64-lgpl-8.1.tar.xz
	ffmpeg-n8.1-latest-linux64-lgpl-shared-8.1.tar.xz
	ffmpeg-n8.1-latest-linuxarm64-gpl-8.1.tar.xz
	ffmpeg-n8.1-latest-linuxarm64-gpl-shared-8.1.tar.xz
	ffmpeg-n8.1-latest-linuxarm64-lgpl-8.1.tar.xz
	ffmpeg-n8.1-latest-linuxarm64-lgpl-shared-8.1.tar.xz
	ffmpeg-n8.1-latest-win64-gpl-8.1.zip
	ffmpeg-n8.1-latest-win64-gpl-shared-8.1.zip
	ffmpeg-n8.1-latest-win64-lgpl-8.1.zip
	ffmpeg-n8.1-latest-win64-lgpl-shared-8.1.zip
	ffmpeg-n8.1-latest-winarm64-gpl-8.1.zip
	ffmpeg-n8.1-latest-winarm64-gpl-shared-8.1.zip
	ffmpeg-n8.1-latest-winarm64-lgpl-8.1.zip
	ffmpeg-n8.1-latest-winarm64-lgpl-shared-8.1.zip
	ffmpeg-n9.0-latest-linux64-gpl-9.0.tar.xz
	ffmpeg-n9.0-latest-linux64-gpl-shared-9.0.tar.xz
	ffmpeg-n9.0-latest-linux64-lgpl-9.0.tar.xz
	ffmpeg-n9.0-latest-linux64-lgpl-shared-9.0.tar.xz
	ffmpeg-n9.0-latest-linuxarm64-gpl-9.0.tar.xz
	ffmpeg-n9.0-latest-linuxarm64-gpl-shared-9.0.tar.xz
	ffmpeg-n9.0-latest-linuxarm64-lgpl-9.0.tar.xz
	ffmpeg-n9.0-latest-linuxarm64-lgpl-shared-9.0.tar.xz
	ffmpeg-n9.0-latest-win64-gpl-9.0.zip
	ffmpeg-n9.0-latest-win64-gpl-shared-9.0.zip
	ffmpeg-n9.0-latest-win64-lgpl-9.0.zip
	ffmpeg-n9.0-latest-win64-lgpl-shared-9.0.zip
	ffmpeg-n9.0-latest-winarm64-gpl-9.0.zip
	ffmpeg-n9.0-latest-winarm64-gpl-shared-9.0.zip
	ffmpeg-n9.0-latest-winarm64-lgpl-9.0.zip
	ffmpeg-n9.0-latest-winarm64-lgpl-shared-9.0.zip
	ASSETS
}

# The full catalogue selects the lowest qualifying linux64-gpl series.
# ffmpeg-n8.1-latest-linuxarm64-gpl-8.1.tar.xz ties on (major, minor) and must lose.
expect_ok "full catalogue" 7 "ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz" < <(catalogue)

# Remove the two qualifying linux64-gpl entries; 47 names remain, none qualifying.
# The count in the message is what tells an operator BtbN retired the series
# rather than the read having failed.
expect_stderr "no qualifying series" 1 "among 47 name(s)" 7 < <(
	catalogue | grep -vx 'ffmpeg-n8\.1-latest-linux64-gpl-8\.1\.tar\.xz' \
	          | grep -vx 'ffmpeg-n9\.0-latest-linux64-gpl-9\.0\.tar\.xz'
)

# Numeric major comparison: n10.0 must not sort below n9.0.
expect_ok "double-digit major" 7 "ffmpeg-n9.0-latest-linux64-gpl-9.0.tar.xz" <<-'EOF'
	ffmpeg-n10.0-latest-linux64-gpl-10.0.tar.xz
	ffmpeg-n9.0-latest-linux64-gpl-9.0.tar.xz
EOF

# Numeric minor comparison. Ascending on purpose: with the higher minor first the
# incumbent is always the one that should lose, so widening (major < best_major)
# to <= would go unnoticed.
expect_ok "double-digit minor" 7 "ffmpeg-n8.9-latest-linux64-gpl-8.9.tar.xz" <<-'EOF'
	ffmpeg-n8.9-latest-linux64-gpl-8.9.tar.xz
	ffmpeg-n8.10-latest-linux64-gpl-8.10.tar.xz
EOF

# master and -shared- are not candidates. Not by a rule the script implements --
# the anchored pattern admits neither.
expect_status "master and shared only" 1 7 <<-'EOF'
	ffmpeg-master-latest-linux64-gpl.tar.xz
	ffmpeg-master-latest-linux64-gpl-shared.tar.xz
	ffmpeg-n8.1-latest-linux64-gpl-shared-8.1.tar.xz
EOF

# Escaped dots, one case per position: a single fixture carrying '/' at several
# positions stays green when only one \. is unescaped.
expect_status "slash at dot 1" 1 7 <<-'EOF'
	ffmpeg-n8/1-latest-linux64-gpl-8.1.tar.xz
EOF
expect_status "slash at dot 2" 1 7 <<-'EOF'
	ffmpeg-n8.1-latest-linux64-gpl-8/1.tar.xz
EOF
expect_status "slash at dot 3" 1 7 <<-'EOF'
	ffmpeg-n8.1-latest-linux64-gpl-8.1/tar.xz
EOF
expect_status "slash at dot 4" 1 7 <<-'EOF'
	ffmpeg-n8.1-latest-linux64-gpl-8.1.tar/xz
EOF

# Whole-line anchoring: an unanchored pattern matches inside both of these, and the
# selected string would carry a path segment or a wrong extension into the URL.
expect_status "unanchored match" 1 7 <<-'EOF'
	evil/ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz
	ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz.sig
EOF

# Capture agreement, split by capture: a fixture disagreeing on both stays green
# when only one of the two checks is deleted.
expect_status "major disagreement only" 1 7 <<-'EOF'
	ffmpeg-n8.1-latest-linux64-gpl-9.1.tar.xz
EOF
expect_status "minor disagreement only" 1 7 <<-'EOF'
	ffmpeg-n8.1-latest-linux64-gpl-8.0.tar.xz
EOF

# The floor: lowest QUALIFYING series, not lowest matching.
expect_ok "below-floor skipped" 7 "ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz" <<-'EOF'
	ffmpeg-n6.1-latest-linux64-gpl-6.1.tar.xz
	ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz
EOF
expect_status "only below-floor" 1 7 <<-'EOF'
	ffmpeg-n6.1-latest-linux64-gpl-6.1.tar.xz
EOF

# The floor's BOUNDARY. Without a major equal to the floor, >= and > are
# indistinguishable -- and "7.0 or later" is exactly what the boundary encodes.
expect_ok "at-floor accepted" 7 "ffmpeg-n7.1-latest-linux64-gpl-7.1.tar.xz" <<-'EOF'
	ffmpeg-n6.1-latest-linux64-gpl-6.1.tar.xz
	ffmpeg-n7.1-latest-linux64-gpl-7.1.tar.xz
	ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz
EOF

# The floor ARGUMENT must reach the comparison, and reach it numerically. Every
# other case passes 7, so a hardcoded >= 7 survives them all; and under string
# collation "9" > "10", so a string compare would let n9.0 qualify and win.
expect_ok "floor drives selection" 10 "ffmpeg-n10.0-latest-linux64-gpl-10.0.tar.xz" <<-'EOF'
	ffmpeg-n9.0-latest-linux64-gpl-9.0.tar.xz
	ffmpeg-n10.0-latest-linux64-gpl-10.0.tar.xz
EOF

# Floor-argument grammar; each exits 2.
expect_status "no argument" 2 </dev/null
expect_status "empty floor" 2 "" </dev/null
expect_status "non-numeric floor" 2 abc </dev/null
expect_status "negative floor" 2 -1 </dev/null
expect_status "dotted floor" 2 7.0 </dev/null
expect_status "zero-padded floor" 2 08 </dev/null
expect_status "two arguments" 2 7 8 </dev/null

# Base 10, not octal. Pad BOTH: a major-only pad leaves the guards on captures
# 2 and 4 unexercised.
expect_ok "zero-padded series" 7 "ffmpeg-n08.08-latest-linux64-gpl-08.08.tar.xz" <<-'EOF'
	ffmpeg-n08.08-latest-linux64-gpl-08.08.tar.xz
	ffmpeg-n9.0-latest-linux64-gpl-9.0.tar.xz
EOF

# A read that yielded nothing is not a retired catalogue. The here-string carries
# real spaces; <<- would strip tabs and collapse a tab-indented line to empty.
expect_stderr "empty stdin" 3 "the release read returned nothing" 7 </dev/null
expect_stderr "empty-line stdin" 3 "the release read returned nothing" 7 <<-'EOF'

EOF
expect_stderr "whitespace-only stdin" 3 "the release read returned nothing" 7 <<<'   '

# The trim must PRESERVE the name it keeps. An all-whitespace fixture pins the two
# trim expansions only as a pair and says nothing about a real name surviving.
expect_ok "surrounding whitespace" 7 "ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz" \
	<<<'  ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz  '

if ((failures > 0)); then
	echo "select-ffmpeg-asset-selftest: $failures case(s) failed" >&2
	exit 1
fi

echo "select-ffmpeg-asset-selftest: OK"
