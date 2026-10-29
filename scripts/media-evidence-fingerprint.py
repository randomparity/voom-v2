#!/usr/bin/env python3
"""Experiment-only partial-fingerprint evaluation for issue #796.

`oracle` runs mutation-oracle cases O1-O13 of docs/media-evidence-protocol.md against each
stage-3 candidate on synthetic fixtures in a scratch directory it creates and removes. `run`
measures each candidate's cost over a private #788 manifest and writes a committed-safe
record. A `Fingerprint` is experiment evidence only: nothing here converts it into, stores it
as, or compares it with production identity evidence. Records carry ids, booleans, counts and
metrics, never digests, inode numbers, paths or file names.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import dataclasses
import errno
import hashlib
import importlib.util
import json
import os
import random
import re
import resource
import shutil
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Callable

_spec = importlib.util.spec_from_file_location(
    "media_evidence_baseline", Path(__file__).with_name("media-evidence-baseline.py")
)
baseline = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(baseline)

ORACLE_SCHEMA = "media-evidence-fingerprint-oracle/1"
RUN_SCHEMA = "media-evidence-fingerprint-run/1"
UNAVAILABLE = baseline.UNAVAILABLE
RECORD_DIR = baseline.REPO_ROOT / "docs" / "media-evidence-baseline" / "fingerprint"
KIB = 1024
MIB = 1024 * KIB
SAMPLE_BYTES = 64 * KIB
# Stage 3 of the protocol: head/tail edge bytes and interior sample count per candidate.
CANDIDATES = {
    "meta": (0, 0),
    "ht64": (64 * KIB, 0),
    "ht1m": (1 * MIB, 0),
    "ht64-i8": (64 * KIB, 8),
    "ht64-i32": (64 * KIB, 32),
}
NETWORK_MAX_FILES = 3
# Larger than every candidate's coverage (ht64-i32 reads 34 x 64 KiB, ht1m 2 MiB), so each
# sampling candidate has unread interior bytes for a constructed miss.
LARGE_BYTES = 8 * MIB
SHORT_BYTES = 100 * KIB
OVERLAP_BYTES = 96 * KIB
SIDECAR_BYTES = 4 * KIB
# An even whole second, so +100 ns and +900 ns share their 1 s and 2 s buckets (O7).
BASE_MTIME_NS = 1_600_000_000 * 1_000_000_000
BUMPED_MTIME_NS = BASE_MTIME_NS + 1_000_000_000
OUTCOMES = {"detected", "missed", "equal", "false-change", "not-applicable", "invalid"}
REFERENCES = {"changed", "unchanged", "not-applicable"}
CONSTRUCTIONS = {
    "O1": "large fixture; same-size edit at the first unread byte past 1 MiB; mtime restored",
    "O2": "large fixture; same-size edit at the first unread byte of the first 64 KiB; "
          "mtime restored",
    "O3": "large fixture; same-size edit at the first unread byte of the last 64 KiB; "
          "mtime restored",
    "O4": "100 KiB fixture; same-size edit at the first unread byte; mtime restored",
    "O5": "empty fixture; one byte appended and truncated; mtime set 1 s later",
    "O6": "96 KiB fixture; edit at 48 KiB, inside the head/tail and sample overlap; "
          "mtime restored",
    "O7": "large fixture; edit at the first unread interior byte; mtime moves +100 ns to "
          "+900 ns within one 2 s bucket",
    "O8": "large fixture renamed within its directory; bytes unchanged",
    "O9": "replacement written under a temporary name with an interior edit and the same "
          "size and mtime, renamed over the still-open original",
    "O10": "a byte already read is changed and mtime set 1 s later between two reads",
    "O11": "fingerprint, then an interior edit with mtime set 1 s later, then re-fingerprint "
           "before use",
    "O12": "4 KiB sidecar beside an unchanged primary; sidecar edit at the first unread byte; "
           "mtime restored",
    "O13": "fingerprint-keyed cache of the reference; interior edit with mtime restored; "
           "lookup after the change",
}
EXCLUDED_CASES = {"O14": "#797 lazy hash lifecycle", "O15": "#797 lazy hash lifecycle"}
NOT_MEASURED = {
    "first_durable_observation": "the experiment does not publish",
    "per_file_physical_bytes": "process I/O counters cover a whole repetition",
    "peak_memory": "fingerprints run in the harness process, which has no worker to sample",
    "probe": "fingerprints replace no probe; ffprobe is not run",
}
PRIVATE_KEYS = {"sample_digest", "file_key", "mtime_ns", "dev", "ino", "relative", "root", "path"}
DIGEST_LIKE = re.compile(r"[0-9a-f]{32,}")


@dataclasses.dataclass(frozen=True)
class Fingerprint:
    """Partial evidence for one file under one candidate; comparable only with its own kind."""

    candidate: str
    size: int
    mtime_ns: int
    file_key: tuple[int, int]
    sample_digest: str


class Drift(Exception):
    """File facts changed between the pre-read and post-read stats."""


class PreconditionFailed(Exception):
    """An oracle case was not constructed as its definition requires."""


def sample_plan(candidate: str, size: int) -> list[tuple[int, int]]:
    """Merged, clipped [start, end) ranges a candidate reads from a file of `size` bytes."""
    edge, interior = CANDIDATES[candidate]
    if edge == 0 or size == 0:
        return []
    ranges = [(0, min(edge, size)), (max(0, size - edge), size)]
    for index in range(1, interior + 1):
        start = index * size // (interior + 1) - SAMPLE_BYTES // 2
        start = min(max(0, start), max(0, size - SAMPLE_BYTES))
        ranges.append((start, min(size, start + SAMPLE_BYTES)))
    merged: list[tuple[int, int]] = []
    for start, end in sorted(ranges):
        if merged and start <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(merged[-1][1], end))
        else:
            merged.append((start, end))
    return merged


def stat_facts(facts: os.stat_result) -> tuple[int, ...]:
    """The fields the hash worker's drift check compares."""
    return (facts.st_size, facts.st_mtime_ns, facts.st_dev, facts.st_ino, facts.st_nlink)


def read_range(descriptor: int, start: int, end: int) -> bytes:
    chunks, offset = [], start
    while offset < end:
        chunk = os.pread(descriptor, end - offset, offset)
        if not chunk:
            break
        chunks.append(chunk)
        offset += len(chunk)
    return b"".join(chunks)


def fingerprint(path: Path, candidate: str,
                between_reads: Callable[[], None] | None = None) -> tuple[Fingerprint, int]:
    """Return the fingerprint and the bytes read; `between_reads` runs after the first range."""
    if candidate == "meta":
        facts = os.stat(path)
        return make_fingerprint(candidate, facts, ""), 0
    descriptor = os.open(path, os.O_RDONLY)
    try:
        before = os.fstat(descriptor)
        digest, total = hashlib.blake2b(digest_size=32), 0
        for start, end in sample_plan(candidate, before.st_size):
            data = read_range(descriptor, start, end)
            digest.update(start.to_bytes(8, "little") + len(data).to_bytes(8, "little"))
            digest.update(data)
            total += len(data)
            if between_reads is not None:
                between_reads()
                between_reads = None
        after = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    if stat_facts(before) != stat_facts(after):
        raise Drift(candidate)
    return make_fingerprint(candidate, before, digest.hexdigest()), total


def make_fingerprint(candidate: str, facts: os.stat_result, digest: str) -> Fingerprint:
    return Fingerprint(candidate, facts.st_size, facts.st_mtime_ns,
                       (facts.st_dev, facts.st_ino), digest)


# ---- mutation oracle -------------------------------------------------------------------


def set_mtime(path: Path, mtime_ns: int) -> None:
    os.utime(path, ns=(mtime_ns, mtime_ns))


def reference_digest(path: Path) -> str:
    """Whole-file digest that proves whether a case's bytes changed."""
    digest = hashlib.blake2b(digest_size=32)
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(MIB), b""):
            digest.update(block)
    return digest.hexdigest()


def write_fixture(path: Path, data: bytes) -> None:
    path.write_bytes(data)
    set_mtime(path, BASE_MTIME_NS)


def first_unread(candidate: str, size: int, low: int, high: int) -> int:
    """First offset in [low, high) the candidate does not read, else `low`."""
    offset = low
    for start, end in sample_plan(candidate, size):
        if start <= offset < end:
            offset = end
    return offset if offset < high else low


def flip_byte(path: Path, offset: int) -> None:
    with path.open("r+b") as handle:
        handle.seek(offset)
        value = handle.read(1)
        handle.seek(offset)
        handle.write(bytes([value[0] ^ 0xFF]))


def require(condition: bool, reason: str) -> None:
    if not condition:
        raise PreconditionFailed(reason)


def edit_restoring_mtime(path: Path, offset: int) -> None:
    before = os.stat(path).st_mtime_ns
    flip_byte(path, offset)
    set_mtime(path, before)
    require(os.stat(path).st_mtime_ns == before, "mtime_not_restored")


class Oracle:
    """Builds each case in its own directory under one scratch root."""

    def __init__(self, scratch: Path, large: bytes):
        self.scratch = scratch
        self.large = large
        self.cases = {
            "O1": self.interior_edit, "O2": self.head_edit, "O3": self.tail_edit,
            "O4": self.short_file, "O5": self.empty_file, "O6": self.overlap,
            "O7": self.timestamp_precision, "O8": self.rename, "O9": self.replacement,
            "O10": self.change_during_observation, "O11": self.change_during_use,
            "O12": self.sidecar_change, "O13": self.stale_cache,
        }

    def run(self, case: str, candidate: str) -> dict[str, Any]:
        directory = self.scratch / f"{case}-{candidate}"
        directory.mkdir()
        try:
            outcome, reference = self.cases[case](directory, candidate)
            reason = None
        except (PreconditionFailed, Drift, OSError) as error:
            outcome, reference = "invalid", "not-applicable"
            reason = failure_code(error)
        finally:
            shutil.rmtree(directory)
        return {"case": case, "candidate": candidate, "outcome": outcome,
                "reference": reference, "reason": reason}

    def compare(self, path: Path, candidate: str, change: Callable[[Path], Path],
                expect_change: bool) -> tuple[str, str]:
        """Fingerprint, apply `change` (which returns the path to re-observe), and classify."""
        before, reference = fingerprint(path, candidate)[0], reference_digest(path)
        after_path = change(path)
        after = fingerprint(after_path, candidate)[0]
        changed = reference_digest(after_path) != reference
        require(changed == expect_change, "reference_mismatch")
        return classify(changed, before == after), "changed" if changed else "unchanged"

    def large_fixture(self, directory: Path) -> Path:
        path = directory / "primary.mkv"
        write_fixture(path, self.large)
        return path

    def region_edit(self, directory: Path, candidate: str, low: int, high: int) -> tuple:
        path = self.large_fixture(directory)
        offset = first_unread(candidate, len(self.large), low, high)
        return self.compare(path, candidate, lambda p: edit_and_return(p, offset), True)

    def interior_edit(self, directory, candidate):
        return self.region_edit(directory, candidate, MIB, len(self.large) - MIB)

    def head_edit(self, directory, candidate):
        return self.region_edit(directory, candidate, 0, SAMPLE_BYTES)

    def tail_edit(self, directory, candidate):
        size = len(self.large)
        return self.region_edit(directory, candidate, size - SAMPLE_BYTES, size)

    def short_file(self, directory, candidate):
        path = directory / "short.mkv"
        write_fixture(path, self.large[:SHORT_BYTES])
        offset = first_unread(candidate, SHORT_BYTES, 0, SHORT_BYTES)
        return self.compare(path, candidate, lambda p: edit_and_return(p, offset), True)

    def empty_file(self, directory, candidate):
        path = directory / "empty.mkv"
        write_fixture(path, b"")

        def append_and_truncate(target: Path) -> Path:
            with target.open("ab") as handle:
                handle.write(b"x")
            os.truncate(target, 0)
            set_mtime(target, BUMPED_MTIME_NS)
            return target

        return self.compare(path, candidate, append_and_truncate, False)

    def overlap(self, directory, candidate):
        path = directory / "overlap.mkv"
        write_fixture(path, self.large[:OVERLAP_BYTES])
        return self.compare(path, candidate, lambda p: edit_and_return(p, OVERLAP_BYTES // 2), True)

    def timestamp_precision(self, directory, candidate):
        path = self.large_fixture(directory)
        set_mtime(path, BASE_MTIME_NS + 100)
        offset = first_unread(candidate, len(self.large), MIB, len(self.large) - MIB)

        def edit_within_bucket(target: Path) -> Path:
            flip_byte(target, offset)
            set_mtime(target, BASE_MTIME_NS + 900)
            observed = os.stat(target).st_mtime_ns
            require(observed == BASE_MTIME_NS + 900, "mtime_relation")
            return target

        return self.compare(path, candidate, edit_within_bucket, True)

    def rename(self, directory, candidate):
        path = self.large_fixture(directory)

        def move(target: Path) -> Path:
            moved = target.with_name("renamed.mkv")
            os.rename(target, moved)
            return moved

        return self.compare(path, candidate, move, False)

    def replacement(self, directory, candidate):
        path = self.large_fixture(directory)
        offset = first_unread(candidate, len(self.large), MIB, len(self.large) - MIB)
        held = os.open(path, os.O_RDONLY)
        try:
            def replace(target: Path) -> Path:
                temporary = target.with_name("replacement.tmp")
                write_fixture(temporary, self.large)
                flip_byte(temporary, offset)
                set_mtime(temporary, BASE_MTIME_NS)
                os.rename(temporary, target)
                facts = os.stat(target)
                require(facts.st_ino != os.fstat(held).st_ino, "inode_reused")
                require(facts.st_mtime_ns == BASE_MTIME_NS, "mtime_relation")
                return target

            return self.compare(path, candidate, replace, True)
        finally:
            os.close(held)

    def change_during_observation(self, directory, candidate):
        if candidate == "meta":
            return "not-applicable", "not-applicable"
        path = self.large_fixture(directory)
        reference = reference_digest(path)

        def write_behind_reader() -> None:
            flip_byte(path, 0)
            set_mtime(path, BUMPED_MTIME_NS)

        try:
            fingerprint(path, candidate, between_reads=write_behind_reader)
            outcome = "missed"
        except Drift:
            outcome = "detected"
        require(reference_digest(path) != reference, "reference_mismatch")
        return outcome, "changed"

    def change_during_use(self, directory, candidate):
        path = self.large_fixture(directory)
        offset = first_unread(candidate, len(self.large), MIB, len(self.large) - MIB)

        def ordinary_write(target: Path) -> Path:
            flip_byte(target, offset)
            set_mtime(target, BUMPED_MTIME_NS)
            require(os.stat(target).st_mtime_ns == BUMPED_MTIME_NS, "mtime_relation")
            return target

        return self.compare(path, candidate, ordinary_write, True)

    def sidecar_change(self, directory, candidate):
        primary = self.large_fixture(directory)
        sidecar = directory / "primary.srt"
        write_fixture(sidecar, self.large[:SIDECAR_BYTES])
        primary_before = fingerprint(primary, candidate)[0]
        offset = first_unread(candidate, SIDECAR_BYTES, 0, SIDECAR_BYTES)
        result = self.compare(sidecar, candidate, lambda p: edit_and_return(p, offset), True)
        require(fingerprint(primary, candidate)[0] == primary_before, "primary_changed")
        return result

    def stale_cache(self, directory, candidate):
        path = self.large_fixture(directory)
        cache = {fingerprint(path, candidate)[0]: reference_digest(path)}
        offset = first_unread(candidate, len(self.large), MIB, len(self.large) - MIB)
        edit_restoring_mtime(path, offset)
        cached = cache.get(fingerprint(path, candidate)[0])
        current = reference_digest(path)
        require(current not in cache.values(), "reference_mismatch")
        return ("missed" if cached is not None else "detected"), "changed"


def edit_and_return(path: Path, offset: int) -> Path:
    edit_restoring_mtime(path, offset)
    return path


def classify(changed: bool, same_fingerprint: bool) -> str:
    if changed:
        return "missed" if same_fingerprint else "detected"
    return "equal" if same_fingerprint else "false-change"


def failure_code(error: BaseException) -> str:
    if isinstance(error, PreconditionFailed):
        return str(error)
    if isinstance(error, Drift):
        return "unexpected_drift"
    return errno.errorcode.get(getattr(error, "errno", None) or 0, "io_error")


def run_oracle(scratch_parent: Path, candidates: list[str] | None = None) -> dict[str, Any]:
    candidates = candidates or list(CANDIDATES)
    large = random.Random(796).randbytes(LARGE_BYTES)
    scratch = Path(tempfile.mkdtemp(prefix="fingerprint-oracle-", dir=scratch_parent))
    try:
        oracle = Oracle(scratch, large)
        rows = [oracle.run(case, candidate) for case in CONSTRUCTIONS for candidate in candidates]
        scratch_type = baseline.filesystem_type(scratch)
    finally:
        shutil.rmtree(scratch)
    return {
        "schema": ORACLE_SCHEMA,
        "build": build_identity(),
        "host": baseline.host_identity(),
        "parameters": {
            "candidates": candidate_parameters(candidates),
            "large_fixture_bytes": LARGE_BYTES, "short_fixture_bytes": SHORT_BYTES,
            "overlap_fixture_bytes": OVERLAP_BYTES, "sidecar_bytes": SIDECAR_BYTES,
            "scratch_filesystem_type": scratch_type,
        },
        "constructions": CONSTRUCTIONS,
        "excluded_cases": EXCLUDED_CASES,
        "cases": rows,
    }


# ---- corpus cost runs ------------------------------------------------------------------


def check_corpus_bounds(storage: str, file_count: int, evict: bool) -> None:
    """Protocol budgets: eviction is local-only and a non-local subset is at most 3 files."""
    if storage == "local":
        return
    if evict:
        raise baseline.HarnessError(f"refusing --evict on {storage} storage")
    if file_count > NETWORK_MAX_FILES:
        raise baseline.HarnessError(
            f"{storage} storage allows at most {NETWORK_MAX_FILES} files; select ids with --ids"
        )


def process_io() -> dict[str, int | str]:
    counters: dict[str, int | str] = {"rchar": UNAVAILABLE, "read_bytes": UNAVAILABLE}
    try:
        for line in Path("/proc/self/io").read_text(encoding="utf-8").splitlines():
            key, value = line.split(":")
            if key in counters:
                counters[key] = int(value)
    except (OSError, ValueError):
        pass
    return counters


def cpu_seconds() -> float:
    usage = resource.getrusage(resource.RUSAGE_SELF)
    return usage.ru_utime + usage.ru_stime


def observe(entry: dict[str, Any], root: Path, candidate: str) -> tuple[dict[str, Any], Any]:
    row: dict[str, Any] = {
        "id": entry["id"], "size_bytes": entry["size_bytes"], "ext": entry["ext"],
        "outcome": "observed", "failure": None, "logical_bytes": 0,
    }
    begin = time.monotonic()
    try:
        observed, row["logical_bytes"] = fingerprint(root / entry["relative"], candidate)
    except (Drift, OSError) as error:
        observed = None
        failure_class = "drift" if isinstance(error, Drift) else "io"
        row.update(outcome="failed", failure={
            "stage": "fingerprint", "class": failure_class, "code": failure_code(error),
        })
    row["seconds"] = round(time.monotonic() - begin, 6)
    return row, observed


def run_repetition(index: int, candidate: str, root: Path, files: list[dict[str, Any]],
                   args: argparse.Namespace, fs_type: str) -> tuple[dict[str, Any], list]:
    paths = {entry["id"]: root / entry["relative"] for entry in files}
    if args.evict:
        for path in paths.values():
            baseline.evict(path, fs_type)
    residency = {file_id: baseline.resident_bytes(path) for file_id, path in paths.items()}
    counters, io_before, cpu_before = baseline.host_counters(), process_io(), cpu_seconds()
    started = time.monotonic()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.concurrency) as pool:
        results = list(pool.map(lambda entry: observe(entry, root, candidate), files))
    wall = time.monotonic() - started
    cpu = cpu_seconds() - cpu_before
    io_delta = baseline.counter_delta(io_before, process_io())
    counters = baseline.counter_delta(counters, baseline.host_counters())
    rows = [row for row, _ in results]
    for row in rows:
        row["resident_bytes_before"] = residency[row["id"]]
    repetition = {
        "index": index, "evicted": args.evict, "wall_seconds": round(wall, 6),
        "cpu_seconds": round(cpu, 6),
        "logical_bytes_read": sum(row["logical_bytes"] for row in rows),
        "process_io": io_delta, "host_counters": counters, "files": rows,
    }
    return repetition, [observed for _, observed in results]


def observed_counts(observations: list[list[Any]]) -> dict[str, int]:
    """Stability across repetitions and distinctness across files, as counts only.

    Distinctness compares size and sampled bytes only: the inode and mtime differ between any
    two files, so counting them would make every file distinct by construction.
    """
    per_file = list(zip(*observations))
    failed = sum(1 for values in per_file if any(value is None for value in values))
    stable = sum(
        1 for values in per_file
        if all(value is not None for value in values) and len(set(values)) == 1
    )
    first = [value for value in observations[0] if value is not None]
    return {"files": len(per_file), "failed": failed, "stable": stable,
            "distinct_content": len({(value.size, value.sample_digest) for value in first})}


def capture(args: argparse.Namespace) -> dict[str, Any]:
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    root = Path(manifest["root"])
    files = baseline.select_files(manifest, args.ids)
    fs_type = baseline.corpus_filesystem(root, files)
    storage = baseline.storage_class(fs_type)
    check_corpus_bounds(storage, len(files), args.evict)
    args.concurrency = args.concurrency or baseline.read_constants(baseline.REPO_ROOT)[
        "max_in_flight"]
    build = build_identity()
    cells = []
    for candidate in args.candidates:
        repetitions, observations = [], []
        for index in range(1, args.repetitions + 1):
            repetition, observed = run_repetition(index, candidate, root, files, args, fs_type)
            repetitions.append(repetition)
            observations.append(observed)
        cells.append({"candidate": candidate, "repetitions": repetitions,
                      "observed": observed_counts(observations)})
    return {
        "schema": RUN_SCHEMA,
        "corpus": {
            "label": manifest["label"], "filesystem_type": fs_type, "storage_class": storage,
            "manifest_files": len(manifest["files"]),
            "selected_ids": [entry["id"] for entry in files],
        },
        "build": build,
        "host": baseline.host_identity(),
        "parameters": {
            "candidates": candidate_parameters(args.candidates),
            "concurrency": args.concurrency, "repetitions": args.repetitions,
            "evict": args.evict,
        },
        "cells": cells,
        "not_measured": NOT_MEASURED,
    }


# ---- identity, validation, CLI ---------------------------------------------------------


def candidate_parameters(candidates: list[str]) -> dict[str, dict[str, int]]:
    return {
        name: {"edge_bytes": CANDIDATES[name][0], "interior_samples": CANDIDATES[name][1],
               "sample_bytes": SAMPLE_BYTES if CANDIDATES[name][1] else 0}
        for name in candidates
    }


def build_identity() -> dict[str, Any]:
    commit = baseline.git_output("rev-parse", "HEAD")
    return {
        "git_commit": commit.strip() if commit else UNAVAILABLE,
        "git_changed_paths": baseline.tracked_changes(),
        "script_sha256": baseline.sha256_file(Path(__file__).resolve()),
        "helper_sha256": baseline.sha256_file(Path(_spec.origin).resolve()),
    }


def privacy_problems(record: dict[str, Any]) -> list[str]:
    """No path, inode, mtime or digest may be committed; build hashes are the one exception."""
    problems = []

    def walk(value: Any) -> None:
        if isinstance(value, dict):
            for key, item in value.items():
                if key in PRIVATE_KEYS:
                    problems.append(f"private field {key}")
                walk(item)
        elif isinstance(value, list):
            for item in value:
                walk(item)
        elif isinstance(value, str) and DIGEST_LIKE.search(value):
            problems.append("digest-like value")

    walk({key: value for key, value in record.items() if key != "build"})
    if re.search(r'"/|\\\\|/home/|/Users/|/mnt/', json.dumps(record)):
        problems.append("host path")
    return problems


BUILD_KEYS = {"git_commit", "git_changed_paths", "script_sha256", "helper_sha256"}
HOST_KEYS = {"os", "machine", "cpu_count", "memory_bytes", "python"}
CASE_KEYS = {"case", "candidate", "outcome", "reference", "reason"}


def case_problems(row: dict[str, Any], candidates: set[str]) -> list[str]:
    problems = baseline.missing("case", row, CASE_KEYS)
    if row.get("case") not in CONSTRUCTIONS or row.get("candidate") not in candidates:
        problems.append("case identity")
    if row.get("outcome") not in OUTCOMES or row.get("reference") not in REFERENCES:
        problems.append("case outcome")
    return problems


def validate_oracle(record: dict[str, Any]) -> list[str]:
    problems = [] if record.get("schema") == ORACLE_SCHEMA else ["schema"]
    for section in ("build", "host", "parameters", "constructions", "excluded_cases", "cases"):
        if section not in record:
            problems.append(f"missing {section}")
    if problems:
        return problems
    problems += baseline.missing("build", record["build"], BUILD_KEYS)
    problems += baseline.missing("host", record["host"], HOST_KEYS)
    candidates = set(record["parameters"].get("candidates", {}))
    if not candidates <= set(CANDIDATES):
        problems.append("candidates")
    for row in record["cases"]:
        problems += case_problems(row, candidates)
    seen = {(row.get("case"), row.get("candidate")) for row in record["cases"]}
    if seen != {(case, name) for case in CONSTRUCTIONS for name in candidates}:
        problems.append("case coverage")
    return problems + privacy_problems(record)


REPETITION_KEYS = {"index", "evicted", "wall_seconds", "cpu_seconds", "logical_bytes_read",
                   "process_io", "host_counters", "files"}
ROW_KEYS = {"id", "size_bytes", "ext", "outcome", "failure", "logical_bytes", "seconds",
            "resident_bytes_before"}
OBSERVED_KEYS = {"files", "failed", "stable", "distinct_content"}


def row_problems(row: dict[str, Any]) -> list[str]:
    problems = baseline.missing("file", row, ROW_KEYS)
    if not re.fullmatch(r"f\d{4}", str(row.get("id", ""))):
        problems.append("file id")
    for key in ("size_bytes", "logical_bytes", "seconds", "resident_bytes_before"):
        if not baseline.is_metric(row.get(key)):
            problems.append(f"file {key}")
    failure = row.get("failure")
    if row.get("outcome") == "observed" and failure is not None:
        problems.append("file failure")
    if row.get("outcome") == "failed" and (
        not isinstance(failure, dict) or set(failure) != {"stage", "class", "code"}
    ):
        problems.append("file failure")
    if row.get("outcome") not in ("observed", "failed"):
        problems.append("file outcome")
    return problems


def repetition_problems(repetition: dict[str, Any]) -> list[str]:
    problems = baseline.missing("repetition", repetition, REPETITION_KEYS)
    for key in ("wall_seconds", "cpu_seconds", "logical_bytes_read"):
        if not baseline.is_metric(repetition.get(key)):
            problems.append(f"repetition {key}")
    for group in ("process_io", "host_counters"):
        values = repetition.get(group)
        if not isinstance(values, dict) or not values:
            problems.append(f"repetition {group}")
            continue
        problems += [f"{group} metric" for value in values.values()
                     if not baseline.is_metric(value)]
    rows = repetition.get("files") or []
    if not rows:
        problems.append("repetition files")
    for row in rows:
        problems += row_problems(row)
    return problems


def cell_problems(cell: dict[str, Any]) -> list[str]:
    problems = baseline.missing("observed", cell.get("observed"), OBSERVED_KEYS)
    if cell.get("candidate") not in CANDIDATES:
        problems.append("cell candidate")
    if not cell.get("repetitions"):
        problems.append("cell repetitions")
    for repetition in cell.get("repetitions") or []:
        problems += repetition_problems(repetition)
    return problems


def validate_run(record: dict[str, Any]) -> list[str]:
    problems = [] if record.get("schema") == RUN_SCHEMA else ["schema"]
    for section in ("corpus", "build", "host", "parameters", "cells", "not_measured"):
        if section not in record:
            problems.append(f"missing {section}")
    if problems:
        return problems
    problems += baseline.missing("corpus", record["corpus"], {
        "label", "filesystem_type", "storage_class", "manifest_files", "selected_ids"})
    problems += baseline.missing("build", record["build"], BUILD_KEYS)
    problems += baseline.missing("host", record["host"], HOST_KEYS)
    problems += baseline.missing("parameters", record["parameters"], {
        "candidates", "concurrency", "repetitions", "evict"})
    if set(record["not_measured"]) != set(NOT_MEASURED):
        problems.append("not_measured")
    if not record["cells"]:
        problems.append("cells")
    for cell in record["cells"]:
        problems += cell_problems(cell)
    return problems + privacy_problems(record)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    oracle = commands.add_parser("oracle", help="run mutation-oracle cases O1-O13")
    oracle.add_argument("--output", type=Path, required=True)
    oracle.add_argument("--scratch-parent", type=Path, default=Path(tempfile.gettempdir()))
    run = commands.add_parser("run", help="measure candidate cost over a manifest subset")
    run.add_argument("manifest", type=Path)
    run.add_argument("--output", type=Path, required=True)
    run.add_argument("--ids", help="comma-separated manifest ids; default is every file")
    run.add_argument("--repetitions", type=int, default=3)
    run.add_argument("--concurrency", type=int, help="default: source MAX_IN_FLIGHT")
    run.add_argument("--candidates", type=lambda text: text.split(","),
                     default=list(CANDIDATES))
    run.add_argument("--evict", action="store_true",
                     help="drop page cache for selected files first (local storage only)")
    args = parser.parse_args(argv)
    unknown = set(getattr(args, "candidates", [])) - set(CANDIDATES)
    if unknown:
        parser.error(f"unknown candidates: {','.join(sorted(unknown))}")
    return args


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        if args.command == "oracle":
            record = run_oracle(args.scratch_parent)
            problems = validate_oracle(record)
        else:
            record = capture(args)
            problems = validate_run(record)
        if problems:
            raise baseline.HarnessError("record failed validation: " + ", ".join(problems))
        baseline.write_new(args.output, record, 0o644)
    except (baseline.HarnessError, OSError, ValueError, KeyError) as error:
        print(f"media-evidence-fingerprint: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
