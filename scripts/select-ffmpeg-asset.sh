#!/usr/bin/env bash
# Choose which BtbN FFmpeg-Builds asset the chaos-e2e job should install.
#
# Reads candidate asset names on stdin, one per line, and writes the chosen name
# to stdout. Chooses the lowest published release series whose major version meets
# the floor, so the workflow tracks BtbN's series rotation without naming a series
# it would have to keep up to date.
# See docs/adr/0078-runtime-resolved-ffmpeg-series.md.
set -euo pipefail
# glibc resolves a bracket RANGE like [0-9] through the locale's collation, so under
# a UTF-8 locale it admits Arabic-Indic and fullwidth digits -- which then blow up in
# `10#`. The POSIX class below is ASCII-only in every locale; pinning LC_ALL as well
# covers the [![:space:]] trim globs and removes the dependency on the runner's
# unset default.
export LC_ALL=C

if (($# != 1)); then
	echo "select-ffmpeg-asset: usage: select-ffmpeg-asset.sh <major-version-floor>" >&2
	exit 2
fi

floor_raw=$1
# Reject a zero-padded floor as well: bash reads 08 as an invalid octal literal.
if [[ ! $floor_raw =~ ^[[:digit:]]+$ ]] || [[ $floor_raw =~ ^0[[:digit:]] ]]; then
	echo "select-ffmpeg-asset: floor must be an unpadded non-negative integer, got: $floor_raw" >&2
	exit 2
fi
# The ^0[0-9] reject above is what keeps this out of octal; no 10# needed here.
floor=$floor_raw

# Static GPL linux64 builds of a numbered release series. Anchored whole-line, with
# escaped dots: an unescaped '.' matches '/', and an unanchored pattern matches
# inside a longer name -- either would let the chosen string leave the release path.
# This admits neither the `master` build nor the `-shared-` variants, so there is
# deliberately no separate exclusion rule below.
pattern='^ffmpeg-n([[:digit:]]+)\.([[:digit:]]+)-latest-linux64-gpl-([[:digit:]]+)\.([[:digit:]]+)\.tar\.xz$'

considered=0
best_major=0
best_minor=0
best_asset=

while IFS= read -r line || [[ -n $line ]]; do
	# Trim surrounding whitespace; skip blank lines.
	line=${line#"${line%%[![:space:]]*}"}
	line=${line%"${line##*[![:space:]]}"}
	[[ -n $line ]] || continue
	considered=$((considered + 1))

	[[ $line =~ $pattern ]] || continue

	major=$((10#${BASH_REMATCH[1]}))
	minor=$((10#${BASH_REMATCH[2]}))
	# ERE has no back-references, so the prefix/suffix agreement is checked here.
	# A mismatch means BtbN's naming scheme moved; skip rather than guess.
	((major == 10#${BASH_REMATCH[3]})) || continue
	((minor == 10#${BASH_REMATCH[4]})) || continue

	((major >= floor)) || continue

	if [[ -z $best_asset ]] \
		|| ((major < best_major)) \
		|| ((major == best_major && minor < best_minor)); then
		best_major=$major
		best_minor=$minor
		best_asset=$line
	fi
done

if ((considered == 0)); then
	echo "select-ffmpeg-asset: the release read returned nothing; no asset names on stdin" >&2
	exit 3
fi

if [[ -z $best_asset ]]; then
	echo "select-ffmpeg-asset: no linux64 static GPL asset for a release series >= $floor among $considered name(s)" >&2
	exit 1
fi

printf '%s\n' "$best_asset"
