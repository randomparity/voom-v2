#!/usr/bin/env python3
"""Inventory real media for issue #339 canary selection."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from collections import Counter
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

VIDEO_EXTENSIONS = {
    ".avi",
    ".m2ts",
    ".m4v",
    ".mkv",
    ".mov",
    ".mp4",
    ".mpeg",
    ".mpg",
    ".ts",
    ".webm",
}

DEFAULT_EXCLUDED_DIR_NAMES = {
    ".Trash-1000",
    ".deleted",
    ".stfolder",
    ".syncing_db",
    "@Recently-Snapshot",
}

CANARY_TAG_PRIORITY = [
    "probe_failure",
    "no_audio",
    "no_policy_audio_language_match",
    "non_mkv_container",
    "video_transcode_candidate",
    "surround_audio",
    "multiple_audio_tracks",
    "subtitle_present",
    "attachment_present",
    "defaulted_audio_language",
    "hevc_noop_candidate",
]

REFERENCE_USER_TAG_PRIORITY = [
    "reference_user_current_compatible",
    "non_mkv_container",
    "video_transcode_candidate",
    "surround_audio",
    "multiple_audio_tracks",
    "subtitle_present",
    "attachment_present",
    "hevc_noop_candidate",
]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Read-only ffprobe inventory for issue #339 real-media canaries.",
    )
    parser.add_argument("root", type=Path, help="Media root to inventory.")
    parser.add_argument("--output", type=Path, help="Write JSON report to this path.")
    parser.add_argument(
        "--max-files",
        type=int,
        default=0,
        help="Stop after this many candidate files. Default: no limit.",
    )
    parser.add_argument(
        "--probe-timeout-seconds",
        type=float,
        default=60.0,
        help="Per-file ffprobe timeout. Default: 60.",
    )
    parser.add_argument(
        "--selection-mode",
        choices=["interesting", "reference-user"],
        default="interesting",
        help="Recommendation profile. Default: interesting.",
    )
    parser.add_argument(
        "--sample-per-tag",
        type=int,
        default=2,
        help="Maximum recommended canary files per interest tag. Default: 2.",
    )
    parser.add_argument(
        "--recommend-limit",
        type=int,
        default=24,
        help="Maximum recommended canary files. Default: 24.",
    )
    parser.add_argument(
        "--copy-to",
        type=Path,
        help="Copy recommended canary files to this destination root.",
    )
    parser.add_argument(
        "--copy-max-bytes",
        type=int,
        default=0,
        help="Required with --copy-to. Refuse to copy more than this many bytes.",
    )
    parser.add_argument(
        "--include-hidden-dirs",
        action="store_true",
        help="Include hidden directories and default excluded production dirs.",
    )
    return parser.parse_args()


def ffprobe_version() -> str:
    result = subprocess.run(
        ["ffprobe", "-version"],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit("ffprobe -version failed; install ffprobe or fix PATH")
    return result.stdout.splitlines()[0] if result.stdout else "unknown"


def media_files(root: Path, max_files: int, include_hidden_dirs: bool) -> list[Path]:
    files: list[Path] = []
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames[:] = [
            dirname
            for dirname in sorted(dirnames)
            if should_walk_dir(dirname, include_hidden_dirs)
        ]
        for filename in sorted(filenames):
            if max_files and len(files) >= max_files:
                return files
            path = Path(dirpath) / filename
            if path.suffix.lower() not in VIDEO_EXTENSIONS:
                continue
            if path.is_symlink() or not path.is_file():
                continue
            files.append(path)
    return files


def should_walk_dir(dirname: str, include_hidden_dirs: bool) -> bool:
    if include_hidden_dirs:
        return True
    if dirname in DEFAULT_EXCLUDED_DIR_NAMES:
        return False
    return not dirname.startswith(".")


def run_ffprobe(path: Path, timeout_seconds: float) -> tuple[str, dict[str, Any] | str]:
    command = [
        "ffprobe",
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
        str(path),
    ]
    try:
        result = subprocess.run(
            command,
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout_seconds,
        )
    except subprocess.TimeoutExpired:
        return "timeout", f"ffprobe timed out after {timeout_seconds:g}s"

    if result.returncode != 0:
        return "error", result.stderr.strip() or f"ffprobe exited {result.returncode}"

    try:
        return "ok", json.loads(result.stdout)
    except json.JSONDecodeError as error:
        return "invalid_json", str(error)


def optional_float(value: Any) -> float | None:
    if value is None:
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def stream_language(stream: dict[str, Any]) -> str:
    tags = stream.get("tags")
    if not isinstance(tags, dict):
        return "und"
    language = tags.get("language")
    if not isinstance(language, str) or not language.strip():
        return "und"
    return language.strip().lower()


def stream_disposition(stream: dict[str, Any], name: str) -> bool:
    disposition = stream.get("disposition")
    if not isinstance(disposition, dict):
        return False
    return disposition.get(name) == 1


def summarize_probe(path: Path, root: Path, payload: dict[str, Any]) -> dict[str, Any]:
    streams = payload.get("streams")
    if not isinstance(streams, list):
        streams = []
    format_data = payload.get("format")
    if not isinstance(format_data, dict):
        format_data = {}

    videos = streams_by_type(streams, "video")
    audio = streams_by_type(streams, "audio")
    subtitles = streams_by_type(streams, "subtitle")
    attachments = streams_by_type(streams, "attachment")
    container = str(format_data.get("format_name", "unknown"))
    size = path.stat().st_size
    audio_languages = [stream_language(stream) for stream in audio]
    subtitle_languages = [stream_language(stream) for stream in subtitles]

    record = {
        "path": str(path),
        "relative_path": str(path.relative_to(root)),
        "size_bytes": size,
        "extension": path.suffix.lower(),
        "probe_status": "ok",
        "format_name": container,
        "duration_seconds": optional_float(format_data.get("duration")),
        "bit_rate": optional_float(format_data.get("bit_rate")),
        "video_codecs": codec_names(videos),
        "audio_codecs": codec_names(audio),
        "audio_channels": sorted(channel_counts(audio)),
        "audio_languages": sorted(set(audio_languages)),
        "audio_language_counts": dict(sorted(Counter(audio_languages).items())),
        "audio_default_count": count_disposition(audio, "default"),
        "audio_forced_count": count_disposition(audio, "forced"),
        "subtitle_codecs": codec_names(subtitles),
        "subtitle_languages": sorted(set(subtitle_languages)),
        "subtitle_language_counts": dict(sorted(Counter(subtitle_languages).items())),
        "subtitle_default_count": count_disposition(subtitles, "default"),
        "subtitle_forced_count": count_disposition(subtitles, "forced"),
        "stream_counts": {
            "video": len(videos),
            "audio": len(audio),
            "subtitle": len(subtitles),
            "attachment": len(attachments),
        },
    }
    record["reference_user_current"] = reference_user_current(record)
    record["interest_tags"] = interest_tags(record)
    return record


def streams_by_type(streams: list[Any], codec_type: str) -> list[dict[str, Any]]:
    selected: list[dict[str, Any]] = []
    for stream in streams:
        if isinstance(stream, dict) and stream.get("codec_type") == codec_type:
            selected.append(stream)
    return selected


def codec_names(streams: list[dict[str, Any]]) -> list[str]:
    names: set[str] = set()
    for stream in streams:
        codec = stream.get("codec_name")
        if isinstance(codec, str) and codec:
            names.add(codec.lower())
    return sorted(names)


def channel_counts(streams: list[dict[str, Any]]) -> set[int]:
    counts: set[int] = set()
    for stream in streams:
        channels = stream.get("channels")
        if isinstance(channels, int):
            counts.add(channels)
    return counts


def count_disposition(streams: list[dict[str, Any]], name: str) -> int:
    count = 0
    for stream in streams:
        if stream_disposition(stream, name):
            count += 1
    return count


def reference_user_current(record: dict[str, Any]) -> dict[str, Any]:
    blockers: list[str] = []
    audio_counts = record["audio_language_counts"]
    subtitle_forced_count = int(record["subtitle_forced_count"])
    eng_audio_count = int(audio_counts.get("eng", 0))
    kept_audio_count = int(audio_counts.get("eng", 0)) + int(audio_counts.get("und", 0))
    has_surround_audio = any(channels >= 6 for channels in record["audio_channels"])

    if kept_audio_count == 0:
        blockers.append("no_eng_or_und_audio")
    if eng_audio_count != 1:
        blockers.append("defaults_audio_where_eng_not_exactly_one")
    if subtitle_forced_count != 1:
        blockers.append("defaults_subtitle_where_forced_not_exactly_one")
    if not has_surround_audio:
        blockers.append("downmix_selector_matches_zero_audio")

    return {
        "compatible": not blockers,
        "blockers": blockers,
        "eng_audio_count": eng_audio_count,
        "kept_audio_count": kept_audio_count,
        "forced_subtitle_count": subtitle_forced_count,
        "has_surround_audio": has_surround_audio,
    }


def interest_tags(record: dict[str, Any]) -> list[str]:
    tags: list[str] = []
    format_name = str(record["format_name"])
    video_codecs = set(record["video_codecs"])
    audio_languages = set(record["audio_languages"])
    stream_counts = record["stream_counts"]

    if "matroska" not in format_name:
        tags.append("non_mkv_container")
    if not stream_counts["video"]:
        tags.append("no_video")
    if not stream_counts["audio"]:
        tags.append("no_audio")
    if stream_counts["audio"] > 1:
        tags.append("multiple_audio_tracks")
    if audio_languages and not audio_languages.intersection({"eng", "und"}):
        tags.append("no_policy_audio_language_match")
    if "und" in audio_languages:
        tags.append("defaulted_audio_language")
    if stream_counts["subtitle"]:
        tags.append("subtitle_present")
    if stream_counts["attachment"]:
        tags.append("attachment_present")
    if any(channels >= 6 for channels in record["audio_channels"]):
        tags.append("surround_audio")
    if video_codecs and not video_codecs.issubset({"hevc", "h265"}):
        tags.append("video_transcode_candidate")
    if video_codecs and "matroska" in format_name and video_codecs.issubset({"hevc", "h265"}):
        tags.append("hevc_noop_candidate")
    if record["reference_user_current"]["compatible"]:
        tags.append("reference_user_current_compatible")
    return tags


def failed_record(path: Path, root: Path, status: str, error: str) -> dict[str, Any]:
    return {
        "path": str(path),
        "relative_path": str(path.relative_to(root)),
        "size_bytes": path.stat().st_size,
        "extension": path.suffix.lower(),
        "probe_status": status,
        "probe_error": error,
        "interest_tags": ["probe_failure"],
    }


def build_inventory(root: Path, args: argparse.Namespace) -> dict[str, Any]:
    version = ffprobe_version()
    records: list[dict[str, Any]] = []
    files = media_files(root, args.max_files, args.include_hidden_dirs)
    print(f"discovered {len(files)} candidate media files", file=sys.stderr)
    for index, path in enumerate(files, 1):
        if index == 1 or index % 100 == 0 or index == len(files):
            print(f"probing {index}/{len(files)}: {path}", file=sys.stderr)
        status, payload = run_ffprobe(path, args.probe_timeout_seconds)
        if status == "ok" and isinstance(payload, dict):
            records.append(summarize_probe(path, root, payload))
        else:
            records.append(failed_record(path, root, status, str(payload)))

    report = {
        "schema_version": "0",
        "tool": "issue_339_media_inventory",
        "generated_at": datetime.now(UTC).isoformat(),
        "root": str(root),
        "ffprobe_version": version,
        "limits": {
            "max_files": args.max_files,
            "probe_timeout_seconds": args.probe_timeout_seconds,
            "selection_mode": args.selection_mode,
            "sample_per_tag": args.sample_per_tag,
            "recommend_limit": args.recommend_limit,
            "include_hidden_dirs": args.include_hidden_dirs,
        },
        "summary": summary(records),
        "recommended_canary": recommended_for_mode(records, args),
        "files": records,
    }
    return report


def recommended_for_mode(
    records: list[dict[str, Any]],
    args: argparse.Namespace,
) -> list[dict[str, Any]]:
    if args.selection_mode == "reference-user":
        compatible = [
            record
            for record in records
            if record.get("reference_user_current", {}).get("compatible") is True
        ]
        return select_recommended(
            compatible,
            args.sample_per_tag,
            args.recommend_limit,
            REFERENCE_USER_TAG_PRIORITY,
        )
    return select_recommended(
        records,
        args.sample_per_tag,
        args.recommend_limit,
        CANARY_TAG_PRIORITY,
    )


def select_recommended(
    records: list[dict[str, Any]],
    sample_per_tag: int,
    recommend_limit: int,
    priority_tags: list[str],
) -> list[dict[str, Any]]:
    selected_paths: set[str] = set()
    selected_media: set[tuple[int, str]] = set()
    selected: list[dict[str, Any]] = []
    for tag in priority_tags:
        tagged = sorted(
            (record for record in records if tag in record["interest_tags"]),
            key=lambda record: (record["size_bytes"], record["relative_path"]),
        )
        for record in tagged[:sample_per_tag]:
            if len(selected) >= recommend_limit:
                return selected
            if record["path"] in selected_paths:
                continue
            media_key = (int(record["size_bytes"]), Path(record["relative_path"]).name)
            if media_key in selected_media:
                continue
            selected_paths.add(record["path"])
            selected_media.add(media_key)
            selected.append(canary_record(record))
    return selected


def canary_record(record: dict[str, Any]) -> dict[str, Any]:
    return {
        "path": record["path"],
        "relative_path": record["relative_path"],
        "size_bytes": record["size_bytes"],
        "probe_status": record["probe_status"],
        "interest_tags": record["interest_tags"],
        "reference_user_current": record.get("reference_user_current"),
    }


def summary(records: list[dict[str, Any]]) -> dict[str, Any]:
    tags: Counter[str] = Counter()
    extensions: Counter[str] = Counter()
    video_codecs: Counter[str] = Counter()
    audio_codecs: Counter[str] = Counter()
    reference_blockers: Counter[str] = Counter()
    total_bytes = 0
    for record in records:
        total_bytes += int(record["size_bytes"])
        extensions.update([str(record["extension"])])
        tags.update(record["interest_tags"])
        video_codecs.update(record.get("video_codecs", []))
        audio_codecs.update(record.get("audio_codecs", []))
        reference = record.get("reference_user_current")
        if isinstance(reference, dict):
            reference_blockers.update(reference.get("blockers", []))
    return {
        "file_count": len(records),
        "total_bytes": total_bytes,
        "probe_failures": tags["probe_failure"],
        "extensions": dict(sorted(extensions.items())),
        "interest_tags": dict(sorted(tags.items())),
        "video_codecs": dict(sorted(video_codecs.items())),
        "audio_codecs": dict(sorted(audio_codecs.items())),
        "reference_user_current_compatible": tags[
            "reference_user_current_compatible"
        ],
        "reference_user_current_blockers": dict(sorted(reference_blockers.items())),
    }


def maybe_copy_canary(
    report: dict[str, Any],
    source_root: Path,
    copy_to: Path | None,
    copy_max_bytes: int,
) -> None:
    if copy_to is None:
        return
    if copy_max_bytes <= 0:
        raise SystemExit("--copy-max-bytes is required with --copy-to")

    destination = copy_to.resolve()
    source = source_root.resolve()
    if destination == source or destination.is_relative_to(source):
        raise SystemExit("--copy-to must not be inside the inventory source root")

    selected = report["recommended_canary"]
    total_bytes = sum(int(record["size_bytes"]) for record in selected)
    if total_bytes > copy_max_bytes:
        raise SystemExit(
            f"recommended canary is {total_bytes} bytes; budget is {copy_max_bytes}",
        )

    copied = []
    for record in selected:
        source_path = Path(record["path"])
        target_path = destination / record["relative_path"]
        if target_path.exists():
            raise SystemExit(f"refusing to overwrite existing file: {target_path}")
        target_path.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source_path, target_path)
        copied.append(
            {
                "source": str(source_path),
                "destination": str(target_path),
                "size_bytes": record["size_bytes"],
            },
        )
    report["copied_canary"] = copied


def write_report(report: dict[str, Any], output: Path | None) -> None:
    encoded = json.dumps(report, indent=2, sort_keys=True)
    if output is None:
        print(encoded)
        return
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(f"{encoded}\n", encoding="utf-8")


def main() -> int:
    args = parse_args()
    root = args.root.resolve()
    if not root.is_dir():
        raise SystemExit(f"inventory root is not a directory: {root}")
    report = build_inventory(root, args)
    maybe_copy_canary(report, root, args.copy_to, args.copy_max_bytes)
    write_report(report, args.output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
