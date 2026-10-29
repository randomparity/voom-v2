#!/usr/bin/env python3
"""Experiment-only random-sampling and change-time measurements for issue #798.

`probability` measures how often a seeded-random sampling candidate detects a single-byte
interior edit with the mtime restored, against the analytic fraction k/M, on synthetic
fixtures in a scratch directory it creates and removes. `ctime-probe` writes one small file
in a fresh scratch directory on each given filesystem and records whether the change time,
inode and inode generation move on a read, a content write, a utime() call, a write with the
mtime restored, a chmod and a rename. Records carry booleans, counts and filesystem types,
never paths, timestamps, inode numbers, generations or seeds. The candidates themselves live
in the #796 fingerprint script.
"""

from __future__ import annotations

import argparse
import errno
import fcntl
import importlib.util
import json
import math
import os
import random
import shutil
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Callable

_spec = importlib.util.spec_from_file_location(
    "media_evidence_fingerprint", Path(__file__).with_name("media-evidence-fingerprint.py")
)
fingerprint = importlib.util.module_from_spec(_spec)
# Dataclass processing looks its defining module up by name.
sys.modules[_spec.name] = fingerprint
_spec.loader.exec_module(fingerprint)
baseline = fingerprint.baseline

PROBABILITY_SCHEMA = "media-evidence-reuse-probability/1"
PROBE_SCHEMA = "media-evidence-reuse-ctime-probe/1"
RECORD_DIR = baseline.REPO_ROOT / "docs" / "media-evidence-baseline" / "reuse"
KIB = 1024
MIB = 1024 * KIB
FIXTURE_SIZES = (8 * MIB, 64 * MIB)
TRIALS = 1000
STANDARD_ERRORS = 3
FRACTIONS = (0.001, 0.01, 0.05, 0.1, 0.25, 0.5)
CHECKS = (1, 10, 100)
PROBE_BYTES = 64 * KIB
SETTLE_SECONDS = 2.0
# _IOR('v', 1, long): the inode generation, which filesystems that support it set at creation.
FS_IOC_GETVERSION = 0x80087601
NOT_MEASURED = {
    "server_side_change": "no access to the file server; only this client's writes are probed",
    "client_set_change_time": "an SMB client can send ChangeTime in SET_INFO; whether the "
                              "server honours it was not tested",
}


def build_identity() -> dict[str, Any]:
    """This script's hash, with the fingerprint module it imports as the helper."""
    return fingerprint.build_identity(Path(__file__), Path(_spec.origin))


# ---- detection probability -------------------------------------------------------------


def flip_restoring_mtime(path: Path, offset: int) -> None:
    before = os.stat(path).st_mtime_ns
    fingerprint.flip_byte(path, offset)
    fingerprint.set_mtime(path, before)


def detection_cell(path: Path, candidate: str, size: int, trials: int,
                   rng: random.Random) -> dict[str, Any]:
    """Edit one uniformly chosen interior byte per trial, then check it under a fresh seed.

    The edit position is drawn before the seed, so the editor cannot know the sampled blocks.
    """
    pool = fingerprint.interior_block_count(size)
    samples = min(fingerprint.ALL_CANDIDATES[candidate][1], pool)
    detected = 0
    for _ in range(trials):
        offset = rng.randrange(fingerprint.SAMPLE_BYTES, (pool + 1) * fingerprint.SAMPLE_BYTES)
        seed = rng.getrandbits(64)
        before = fingerprint.fingerprint(path, candidate, seed=seed)[0]
        flip_restoring_mtime(path, offset)
        after = fingerprint.fingerprint(path, candidate, seed=seed)[0]
        flip_restoring_mtime(path, offset)
        detected += before != after
    expected = samples / pool
    error = math.sqrt(expected * (1 - expected) / trials)
    observed = detected / trials
    return {
        "candidate": candidate, "fixture_bytes": size, "interior_blocks": pool,
        "sampled_blocks": samples, "sampled_fraction": round(expected, 6),
        "trials": trials, "detected": detected, "observed_rate": round(observed, 6),
        "standard_error": round(error, 6),
        "within_bound": abs(observed - expected) <= STANDARD_ERRORS * error,
    }


def analytic_table() -> list[dict[str, Any]]:
    """Per-check detection equals the sampled fraction f; n independent checks give 1-(1-f)^n."""
    return [
        {"sampled_fraction": fraction, "checks": checks,
         "detection": round(1 - (1 - fraction) ** checks, 6)}
        for fraction in FRACTIONS for checks in CHECKS
    ]


def run_probability(scratch_parent: Path, trials: int = TRIALS,
                    sizes: tuple[int, ...] = FIXTURE_SIZES) -> dict[str, Any]:
    scratch = Path(tempfile.mkdtemp(prefix="reuse-probability-", dir=scratch_parent))
    try:
        cells = []
        for size in sizes:
            path = scratch / "fixture.bin"
            fingerprint.write_fixture(path, random.Random(size).randbytes(size))
            for candidate in sorted(fingerprint.RANDOM_CANDIDATES):
                rng = random.Random(f"{size}-{candidate}")
                cells.append(detection_cell(path, candidate, size, trials, rng))
        scratch_type = baseline.filesystem_type(scratch)
    finally:
        shutil.rmtree(scratch)
    return {
        "schema": PROBABILITY_SCHEMA,
        "build": build_identity(),
        "host": baseline.host_identity(),
        "parameters": {"trials": trials, "standard_errors": STANDARD_ERRORS,
                       "sample_bytes": fingerprint.SAMPLE_BYTES,
                       "scratch_filesystem_type": scratch_type},
        "cells": cells,
        "analytic": analytic_table(),
    }


# ---- change-time probe -----------------------------------------------------------------


def generation(path: Path) -> int | None:
    """The inode generation, or None where the filesystem does not expose one."""
    descriptor = os.open(path, os.O_RDONLY)
    try:
        buffer = bytearray(8)
        fcntl.ioctl(descriptor, FS_IOC_GETVERSION, buffer)
        return int.from_bytes(buffer[:4], sys.byteorder)
    except OSError as error:
        if error.errno in (errno.ENOTTY, errno.EOPNOTSUPP, errno.EINVAL, errno.ENOSYS):
            return None
        raise
    finally:
        os.close(descriptor)


def read_all(path: Path) -> Path:
    path.read_bytes()
    return path


def content_write(path: Path) -> Path:
    fingerprint.flip_byte(path, PROBE_BYTES // 2)
    return path


def mtime_restore(path: Path) -> Path:
    fingerprint.set_mtime(path, fingerprint.BASE_MTIME_NS)
    return path


def write_restoring_mtime(path: Path) -> Path:
    flip_restoring_mtime(path, PROBE_BYTES // 2)
    return path


def change_mode(path: Path) -> Path:
    os.chmod(path, 0o600 if os.stat(path).st_mode & 0o044 else 0o644)
    return path


def rename(path: Path) -> Path:
    moved = path.with_name("renamed-" + path.name)
    os.rename(path, moved)
    return moved


# The read is the control: a probe that sees ctime move on a read is not measuring writes.
STEPS: dict[str, Callable[[Path], Path]] = {
    "read": read_all,
    "content-write": content_write,
    "utime": mtime_restore,
    "write-restoring-mtime": write_restoring_mtime,
    "chmod": change_mode,
    "rename": rename,
}
STEP_BYTES_WRITTEN = {"content-write": 1, "write-restoring-mtime": 1}


def generation_change(before: int | None, after: int | None) -> str:
    if before is None or after is None:
        return "unsupported"
    return "changed" if before != after else "unchanged"


def probe_step(path: Path, action: Callable[[Path], Path],
               settle: float) -> tuple[dict[str, Any], Path, list[int]]:
    before, before_generation = os.stat(path), generation(path)
    path = action(path)
    at_once = os.stat(path)
    time.sleep(settle)
    settled = os.stat(path)
    row = {
        "size_unchanged": settled.st_size == before.st_size,
        "mtime_changed": settled.st_mtime_ns != before.st_mtime_ns,
        "ctime_changed_at_once": at_once.st_ctime_ns != before.st_ctime_ns,
        "ctime_changed_settled": settled.st_ctime_ns != before.st_ctime_ns,
        "inode_changed": settled.st_ino != before.st_ino,
        "generation_change": generation_change(before_generation, generation(path)),
    }
    return row, path, [before.st_ctime_ns, at_once.st_ctime_ns, settled.st_ctime_ns]


def probe_filesystem(parent: Path, settle: float) -> dict[str, Any]:
    directory = Path(tempfile.mkdtemp(prefix="reuse-ctime-probe-", dir=parent))
    steps, ctimes, written = [], [], 0
    try:
        path = directory / "probe.bin"
        fingerprint.write_fixture(path, random.Random(798).randbytes(PROBE_BYTES))
        written += PROBE_BYTES
        time.sleep(settle)
        for name, action in STEPS.items():
            row, path, observed = probe_step(path, action, settle)
            steps.append({"step": name, **row})
            ctimes += observed
            written += STEP_BYTES_WRITTEN.get(name, 0)
        fs_type = baseline.filesystem_type(directory)
    finally:
        shutil.rmtree(directory, ignore_errors=True)
        if directory.exists():
            print(f"media-evidence-reuse: scratch not removed: {directory}", file=sys.stderr)
    return {
        "filesystem_type": fs_type, "storage_class": baseline.storage_class(fs_type),
        "bytes_written": written, "scratch_removed": not directory.exists(),
        "ctime_subsecond_nonzero": any(value % 1_000_000_000 for value in ctimes),
        "steps": steps,
    }


def manifest_roots(manifests: list[Path]) -> list[Path]:
    roots = []
    for manifest in manifests:
        roots.append(Path(json.loads(manifest.read_text(encoding="utf-8"))["root"]))
    return roots


def check_outside_corpora(parents: list[Path], roots: list[Path]) -> None:
    """Refuse a scratch parent inside a corpus: corpora are never written."""
    for parent in parents:
        for root in roots:
            if parent.resolve().is_relative_to(root.resolve()):
                raise baseline.HarnessError(
                    "refusing a scratch parent inside a manifest root; pick a directory "
                    "outside every corpus"
                )


def run_ctime_probe(parents: list[Path], manifests: list[Path],
                    settle: float = SETTLE_SECONDS) -> dict[str, Any]:
    check_outside_corpora(parents, manifest_roots(manifests))
    return {
        "schema": PROBE_SCHEMA,
        "build": build_identity(),
        "host": baseline.host_identity(),
        "parameters": {"probe_bytes": PROBE_BYTES, "settle_seconds": settle,
                       "steps": list(STEPS)},
        "filesystems": [probe_filesystem(parent, settle) for parent in parents],
        "not_measured": NOT_MEASURED,
    }


# ---- validation, CLI -------------------------------------------------------------------


CELL_KEYS = {"candidate", "fixture_bytes", "interior_blocks", "sampled_blocks",
             "sampled_fraction", "trials", "detected", "observed_rate", "standard_error",
             "within_bound"}
FILESYSTEM_KEYS = {"filesystem_type", "storage_class", "bytes_written", "scratch_removed",
                   "ctime_subsecond_nonzero", "steps"}
STEP_KEYS = {"step", "size_unchanged", "mtime_changed", "ctime_changed_at_once",
             "ctime_changed_settled", "inode_changed", "generation_change"}
GENERATION_CHANGES = {"changed", "unchanged", "unsupported"}


def common_problems(record: dict[str, Any], schema: str, sections: tuple[str, ...]) -> list[str]:
    problems = [] if record.get("schema") == schema else ["schema"]
    problems += [f"missing {section}" for section in sections if section not in record]
    if problems:
        return problems
    problems += baseline.missing("build", record["build"], fingerprint.BUILD_KEYS)
    problems += baseline.missing("host", record["host"], fingerprint.HOST_KEYS)
    return problems + fingerprint.privacy_problems(record)


def validate_probability(record: dict[str, Any]) -> list[str]:
    problems = common_problems(record, PROBABILITY_SCHEMA,
                               ("build", "host", "parameters", "cells", "analytic"))
    if problems:
        return problems
    for cell in record["cells"]:
        problems += baseline.missing("cell", cell, CELL_KEYS)
        if cell.get("candidate") not in fingerprint.RANDOM_CANDIDATES:
            problems.append("cell candidate")
        if not 0 < cell.get("sampled_blocks", 0) <= cell.get("interior_blocks", 0):
            problems.append("cell sampled blocks")
    if {cell.get("candidate") for cell in record["cells"]} != fingerprint.RANDOM_CANDIDATES:
        problems.append("cell coverage")
    return problems


def step_problems(step: dict[str, Any]) -> list[str]:
    problems = baseline.missing("step", step, STEP_KEYS)
    if step.get("step") not in STEPS:
        problems.append("step name")
    if step.get("generation_change") not in GENERATION_CHANGES:
        problems.append("step generation")
    flags = STEP_KEYS - {"step", "generation_change"}
    problems += [f"step {key}" for key in flags if not isinstance(step.get(key), bool)]
    return problems


def validate_probe(record: dict[str, Any]) -> list[str]:
    problems = common_problems(record, PROBE_SCHEMA,
                               ("build", "host", "parameters", "filesystems", "not_measured"))
    if problems:
        return problems
    if set(record["not_measured"]) != set(NOT_MEASURED):
        problems.append("not_measured")
    if not record["filesystems"]:
        problems.append("filesystems")
    for filesystem in record["filesystems"]:
        problems += baseline.missing("filesystem", filesystem, FILESYSTEM_KEYS)
        if [step.get("step") for step in filesystem.get("steps", [])] != list(STEPS):
            problems.append("filesystem steps")
        for step in filesystem.get("steps", []):
            problems += step_problems(step)
    return problems


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    probability = commands.add_parser("probability", help="random-sampling detection rate")
    probability.add_argument("--output", type=Path, required=True)
    probability.add_argument("--scratch-parent", type=Path,
                             default=Path(tempfile.gettempdir()))
    probe = commands.add_parser("ctime-probe", help="change-time behaviour per filesystem")
    probe.add_argument("--output", type=Path, required=True)
    probe.add_argument("--scratch-parent", type=Path, action="append", required=True,
                       help="a writable directory outside every corpus; repeat per filesystem")
    probe.add_argument("--manifest", type=Path, action="append", required=True,
                       help="a private corpus manifest whose root is refused; repeat per corpus")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        if args.command == "probability":
            record = run_probability(args.scratch_parent)
            problems = validate_probability(record)
        else:
            record = run_ctime_probe(args.scratch_parent, args.manifest)
            problems = validate_probe(record)
        if problems:
            raise baseline.HarnessError("record failed validation: " + ", ".join(problems))
        baseline.write_new(args.output, record, 0o644)
    except (baseline.HarnessError, OSError, ValueError, KeyError) as error:
        print(f"media-evidence-reuse: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
