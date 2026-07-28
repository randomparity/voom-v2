#!/usr/bin/env python3
"""Select a diverse set of video files from the VOOM database for testing.

Uses a greedy set-cover algorithm to maximize coverage of unique features
(containers, codecs, track types, HDR, resolutions, etc.) across the
selected file set.
"""

import argparse
import json
import os
import shutil
import sqlite3
import sys
from collections import defaultdict
from pathlib import Path


def parse_size(s):
    """Parse a human-readable size string (e.g., '5G', '500M') to bytes."""
    s = s.strip().upper()
    multipliers = {"B": 1, "K": 1024, "M": 1024**2, "G": 1024**3, "T": 1024**4}
    if s[-1] in multipliers:
        return int(float(s[:-1]) * multipliers[s[-1]])
    return int(s)


def format_size(n):
    """Format bytes as a human-readable string."""
    if n < 1024:
        return f"{n} B"
    for unit in ("KiB", "MiB", "GiB", "TiB"):
        n /= 1024
        if n < 1024:
            return f"{n:.1f} {unit}"
    return f"{n:.1f} PiB"


def resolution_tier(width, height):
    """Classify resolution into a tier name."""
    if width is None or height is None:
        return None
    h = max(width, height)  # handle rotated video
    v = min(width, height)
    if v >= 2160:
        return "4K+"
    if v >= 1080:
        return "1080p"
    if v >= 720:
        return "720p"
    if v >= 480:
        return "SD"
    return "sub-SD"


def channel_layout_name(channels):
    """Map channel count to a layout name."""
    if channels is None:
        return None
    return {1: "mono", 2: "stereo", 6: "5.1", 8: "7.1"}.get(channels, f"{channels}ch")


def truthy(value):
    """Return True for bool-ish values stored in JSON or SQLite rows."""
    return value is True or value == 1


def maybe_lower(value, fallback="unknown"):
    """Normalize optional strings for feature tokens."""
    if isinstance(value, str) and value:
        return value.lower()
    return fallback


def disposition_flag(stream, name):
    """Read a normalized ffprobe stream disposition flag."""
    disposition = stream.get("disposition")
    if not isinstance(disposition, dict):
        return False
    return truthy(disposition.get(name))


def is_vfr_stream(stream):
    """Best-effort VFR classification from normalized ffprobe facts."""
    avg_frame_rate = stream.get("avg_frame_rate")
    r_frame_rate = stream.get("r_frame_rate")
    return bool(avg_frame_rate and r_frame_rate and avg_frame_rate != r_frame_rate)


def hdr_features(stream):
    """Return an HDR boolean and optional format label from stream facts."""
    pixel_format = maybe_lower(stream.get("pixel_format"), "")
    color_transfer = maybe_lower(stream.get("color_transfer"), "")
    color_primaries = maybe_lower(stream.get("color_primaries"), "")
    profile = maybe_lower(stream.get("profile"), "")

    if "smpte2084" in color_transfer or "bt2020" in color_primaries:
        return True, color_transfer or color_primaries
    if "10le" in pixel_format and ("main 10" in profile or "p010" in pixel_format):
        return True, "10-bit"
    return False, None


def track_tuple_from_stream(stream):
    """Convert a normalized media snapshot stream into the selector tuple."""
    kind = stream.get("kind", "")
    is_hdr, hdr_format = hdr_features(stream)
    return (
        kind,
        stream.get("codec_name"),
        stream.get("language"),
        stream.get("title") or stream.get("handler_name"),
        disposition_flag(stream, "default"),
        disposition_flag(stream, "forced"),
        stream.get("channels"),
        stream.get("channel_layout"),
        stream.get("width"),
        stream.get("height"),
        is_vfr_stream(stream),
        is_hdr,
        hdr_format,
        stream.get("pixel_format"),
    )


def extract_features(file_row, tracks):
    """Extract the set of feature strings for a file and its tracks."""
    features = set()
    fid, path, filename, size, container, duration = file_row

    features.add(f"container:{maybe_lower(container)}")

    audio_count = 0
    subtitle_count = 0
    has_forced = False
    has_commentary = False
    has_attachment = False

    for t in tracks:
        (track_type, codec, language, title, is_default, is_forced,
         channels, channel_layout, width, height, is_vfr, is_hdr,
         hdr_format, pixel_format) = t

        tt = maybe_lower(track_type, "")
        codec_lower = maybe_lower(codec)

        if tt == "video":
            features.add(f"video_codec:{codec_lower}")
            tier = resolution_tier(width, height)
            if tier:
                features.add(f"resolution:{tier}")
            if is_hdr:
                fmt = hdr_format.lower() if hdr_format else "generic"
                features.add(f"hdr:{fmt}")
            else:
                features.add("hdr:none")
            if is_vfr is None:
                pass
            elif is_vfr:
                features.add("framerate:vfr")
            else:
                features.add("framerate:cfr")
        elif tt == "audio":
            features.add(f"audio_codec:{codec_lower}")
            audio_count += 1
            if language:
                features.add(f"audio_language:{language.lower()}")
            layout = channel_layout_name(channels)
            if layout:
                features.add(f"audio_channels:{layout}")
            if is_default:
                features.add("default_audio:yes")
        elif tt in ("subtitle", "subtitles"):
            features.add(f"subtitle_codec:{codec_lower}")
            subtitle_count += 1
            if language:
                features.add(f"subtitle_language:{language.lower()}")
            if is_forced:
                has_forced = True
        elif tt == "attachment":
            has_attachment = True

        if title and "commentary" in title.lower():
            has_commentary = True

    if audio_count > 1:
        features.add("multi_audio:yes")
    if subtitle_count > 1:
        features.add("multi_subtitle:yes")
    if has_forced:
        features.add("forced_subtitle:yes")
    if has_commentary:
        features.add("commentary:yes")
    if has_attachment:
        features.add("attachment:yes")
    blockers = reference_user_current_blockers(tracks)
    if blockers:
        for blocker in blockers:
            features.add(f"reference_user_blocker:{blocker}")
    else:
        features.add("reference_user_current:compatible")

    return features


def reference_user_current_blockers(tracks):
    """Return blockers for the current committed reference-user policy."""
    kept_audio_count = 0
    eng_audio_count = 0
    forced_subtitle_count = 0
    has_surround_audio = False

    for track in tracks:
        track_type, _, language, _, _, is_forced, channels, *_ = track
        track_type = maybe_lower(track_type, "")
        language = maybe_lower(language, "und")
        if track_type == "audio":
            if language in {"eng", "und"}:
                kept_audio_count += 1
            if language == "eng":
                eng_audio_count += 1
            if isinstance(channels, int) and channels >= 6:
                has_surround_audio = True
        elif track_type in {"subtitle", "subtitles"} and is_forced:
            forced_subtitle_count += 1

    blockers = []
    if kept_audio_count == 0:
        blockers.append("no_eng_or_und_audio")
    if eng_audio_count != 1:
        blockers.append("defaults_audio_where_eng_not_exactly_one")
    if forced_subtitle_count != 1:
        blockers.append("defaults_subtitle_where_forced_not_exactly_one")
    if not has_surround_audio:
        blockers.append("downmix_selector_matches_zero_audio")
    return blockers


def query_files(db_path):
    """Query live files and their latest media snapshot from the Voom database."""
    conn = sqlite3.connect(db_path)
    conn.execute("PRAGMA foreign_keys = ON")

    rows = conn.execute("""
        WITH latest_snapshots AS (
            SELECT
                id,
                file_version_id,
                payload,
                ROW_NUMBER() OVER (
                    PARTITION BY file_version_id
                    ORDER BY id DESC
                ) AS snapshot_rank
            FROM media_snapshots
        ),
        live_locations AS (
            SELECT
                file_version_id,
                MIN(value) AS value
            FROM file_locations
            WHERE retired_at IS NULL
            GROUP BY file_version_id
        )
        SELECT
            fv.id,
            ll.value,
            fv.size_bytes,
            ms.payload
        FROM file_versions fv
        JOIN live_locations ll ON ll.file_version_id = fv.id
        JOIN latest_snapshots ms
            ON ms.file_version_id = fv.id
           AND ms.snapshot_rank = 1
        WHERE fv.retired_at IS NULL
        ORDER BY fv.id
    """).fetchall()
    conn.close()

    files = {}
    for row in rows:
        fid, path, size, payload_text = row
        payload = json.loads(payload_text)
        container = payload.get("container")
        if not isinstance(container, dict):
            container = {}
        streams = payload.get("streams")
        if not isinstance(streams, list):
            streams = []

        filename = Path(path).name
        file_info = (
            fid,
            path,
            filename,
            size,
            container.get("format_name", "unknown"),
            container.get("duration_seconds"),
        )
        files[fid] = {
            "info": file_info,
            "tracks": [
                track_tuple_from_stream(stream)
                for stream in streams
                if isinstance(stream, dict)
            ],
        }

    return files


def greedy_select(files, max_files, max_size_bytes):
    """Greedy set-cover: pick the file that adds the most uncovered features."""
    # Pre-compute features for all files
    file_features = {}
    for fid, data in files.items():
        feats = extract_features(data["info"], data["tracks"])
        file_features[fid] = feats

    selected = []
    covered = set()
    total_size = 0
    remaining = set(files.keys())

    while remaining and len(selected) < max_files:
        best_fid = None
        best_new = set()
        best_size = 0

        for fid in remaining:
            new_feats = file_features[fid] - covered
            size = files[fid]["info"][3]  # size field
            # Primary: most new features. Tiebreak: smaller file.
            if (len(new_feats) > len(best_new) or
                    (len(new_feats) == len(best_new) and len(new_feats) > 0
                     and size < best_size)):
                best_fid = fid
                best_new = new_feats
                best_size = size

        if best_fid is None or len(best_new) == 0:
            break  # No file adds new coverage

        file_size = files[best_fid]["info"][3]
        if max_size_bytes is not None and total_size + file_size > max_size_bytes:
            # Skip this file and try others that fit
            remaining.discard(best_fid)
            continue

        selected.append(best_fid)
        covered |= best_new
        total_size += file_size
        remaining.discard(best_fid)

    return selected, covered, total_size


def filter_reference_user_current(files):
    """Keep only files expected to pass current reference-user planning."""
    return {
        fid: data
        for fid, data in files.items()
        if not reference_user_current_blockers(data["tracks"])
    }


def print_table(files, selected, covered, verbose):
    """Print a formatted table of selected files."""
    # Header
    print(f"\nSelected {len(selected)} files:\n")
    cols = [
        ("Filename", 40),
        ("Container", 10),
        ("Size", 10),
        ("Video", 8),
        ("Audio", 10),
        ("Subs", 6),
        ("Resolution", 10),
        ("HDR", 12),
    ]
    header = "  ".join(name.ljust(width) for name, width in cols)
    print(header)
    print("-" * len(header))

    for fid in selected:
        data = files[fid]
        info = data["info"]
        tracks = data["tracks"]
        filename = info[2]
        container = info[4]
        size = format_size(info[3])

        # Gather summary from tracks
        video_codecs = set()
        audio_codecs = set()
        sub_count = 0
        resolution = ""
        hdr_info = ""

        for t in tracks:
            tt = t[0].lower()
            codec = t[1] or "?"
            if tt == "video":
                video_codecs.add(codec.lower())
                tier = resolution_tier(t[8], t[9])
                if tier:
                    resolution = tier
                if t[12]:  # hdr_format
                    hdr_info = t[12]
                elif t[11]:  # is_hdr
                    hdr_info = "HDR"
                else:
                    hdr_info = "SDR"
            elif tt == "audio":
                audio_codecs.add(codec.lower())
            elif tt in ("subtitle", "subtitles"):
                sub_count += 1

        vals = [
            (filename[:40], 40),
            (container, 10),
            (size, 10),
            (",".join(sorted(video_codecs))[:8], 8),
            (",".join(sorted(audio_codecs))[:10], 10),
            (str(sub_count), 6),
            (resolution, 10),
            (hdr_info[:12], 12),
        ]
        print("  ".join(str(v).ljust(w) for v, w in vals))

    if verbose:
        print(f"\nPer-file features:")
        for fid in selected:
            data = files[fid]
            feats = extract_features(data["info"], data["tracks"])
            print(f"  {data['info'][2]}")
            for f in sorted(feats):
                print(f"    - {f}")

    # Coverage report
    categories = defaultdict(set)
    for feat in covered:
        cat, val = feat.split(":", 1)
        categories[cat].add(val)

    print(f"\nCoverage ({len(covered)} unique features):\n")
    for cat in sorted(categories):
        vals = sorted(categories[cat])
        print(f"  {cat}: {', '.join(vals)}")
    print()


def destination_for(src, filename, dest_path, source_root):
    """Build a destination path, preserving source-relative paths when possible."""
    if source_root is None:
        return dest_path / filename
    try:
        relative = Path(src).resolve().relative_to(source_root)
    except ValueError:
        return dest_path / filename
    return dest_path / relative


def copy_files(files, selected, dest, dry_run, source_root):
    """Copy selected files to destination directory."""
    dest_path = Path(dest)
    if not dry_run:
        dest_path.mkdir(parents=True, exist_ok=True)

    total = len(selected)
    planned = set()
    for i, fid in enumerate(selected, 1):
        src = files[fid]["info"][1]  # path
        filename = files[fid]["info"][2]
        dst = destination_for(src, filename, dest_path, source_root)

        # Handle duplicate filenames
        if dst.exists() or dst in planned:
            stem = dst.stem
            suffix = dst.suffix
            counter = 1
            while True:
                dst = dst.with_name(f"{stem}_{counter}{suffix}")
                counter += 1
                if not dst.exists() and dst not in planned:
                    break
        planned.add(dst)

        size = format_size(files[fid]["info"][3])
        prefix = "[DRY RUN] " if dry_run else ""
        print(f"  {prefix}[{i}/{total}] {filename} ({size}) -> {dst}")

        if not dry_run:
            if not Path(src).exists():
                print(f"    WARNING: source file not found, skipping")
                continue
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dst)

    if not dry_run:
        print(f"\nCopied {total} files to {dest_path}")


def main():
    parser = argparse.ArgumentParser(
        prog="select-test-corpus",
        description="Select a diverse set of video files from the VOOM database for testing.",
    )
    parser.add_argument(
        "--db", metavar="PATH",
        default=os.path.expanduser("~/.config/voom/voom.db"),
        help="Path to voom.db (default: ~/.config/voom/voom.db)",
    )
    parser.add_argument(
        "--max-files", type=int, default=20, metavar="N",
        help="Maximum files to select (default: 20)",
    )
    parser.add_argument(
        "--max-size", metavar="SIZE",
        help="Maximum total size, e.g. '5G', '500M' (default: no limit)",
    )
    parser.add_argument(
        "--require-reference-user-current",
        action="store_true",
        help="Only select files compatible with current reference-user.voom blockers",
    )
    parser.add_argument(
        "--copy-to", metavar="DEST",
        help="Copy selected files to DEST directory",
    )
    parser.add_argument(
        "--source-root", metavar="PATH",
        help="Preserve paths relative to PATH when copying",
    )
    parser.add_argument(
        "--dry-run", action="store_true",
        help="Show what would be copied without copying",
    )
    parser.add_argument(
        "--json", action="store_true",
        help="Output results as JSON",
    )
    parser.add_argument(
        "--verbose", action="store_true",
        help="Show per-file feature details",
    )
    args = parser.parse_args()

    db_path = args.db
    if not Path(db_path).exists():
        print(f"Error: database not found at {db_path}", file=sys.stderr)
        sys.exit(1)

    max_size_bytes = parse_size(args.max_size) if args.max_size else None

    files = query_files(db_path)
    if not files:
        print("No files found in database.", file=sys.stderr)
        sys.exit(1)

    original_count = len(files)
    if args.require_reference_user_current:
        files = filter_reference_user_current(files)
        if not files:
            print("No reference-user-compatible files found.", file=sys.stderr)
            sys.exit(1)

    database_line = f"Database: {db_path} ({len(files)} files"
    if len(files) != original_count:
        database_line += f" after filtering from {original_count}"
    database_line += ")"
    print(database_line, file=sys.stderr if args.json else sys.stdout)

    selected, covered, total_size = greedy_select(files, args.max_files, max_size_bytes)

    if not selected:
        print("No files selected.", file=sys.stderr)
        sys.exit(1)

    if args.json:
        result = {
            "total_files_in_db": len(files),
            "selected_count": len(selected),
            "total_size": total_size,
            "total_size_human": format_size(total_size),
            "features_covered": len(covered),
            "files": [],
            "coverage": {},
        }
        categories = defaultdict(list)
        for feat in sorted(covered):
            cat, val = feat.split(":", 1)
            categories[cat].append(val)
        result["coverage"] = dict(categories)

        for fid in selected:
            data = files[fid]
            feats = extract_features(data["info"], data["tracks"])
            result["files"].append({
                "id": fid,
                "path": data["info"][1],
                "filename": data["info"][2],
                "size": data["info"][3],
                "size_human": format_size(data["info"][3]),
                "container": data["info"][4],
                "features": sorted(feats),
            })
        print(json.dumps(result, indent=2))
    else:
        print_table(files, selected, covered, args.verbose)
        print(f"Total size: {format_size(total_size)}")

    if args.copy_to:
        print()
        source_root = Path(args.source_root).resolve() if args.source_root else None
        copy_files(files, selected, args.copy_to, args.dry_run, source_root)


if __name__ == "__main__":
    main()
