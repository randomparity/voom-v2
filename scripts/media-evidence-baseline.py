#!/usr/bin/env python3
"""Read-only capture harness for the issue #788 media evidence baseline.

`manifest` lists video files under a corpus root into a private local file.
`run` replays the current per-candidate scan identity sequence (HashFile, then
ProbeFile with the hash facts as expected facts) against built hash and ffprobe
workers and writes a committed-safe record: corpora by label, files by ordinal
id. See docs/media-evidence-protocol.md for the frozen protocol.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import http.client
import json
import os
import platform
import re
import secrets
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any

SCHEMA = "media-evidence-baseline-run/1"
UNAVAILABLE = "unavailable"
REPO_ROOT = Path(__file__).resolve().parents[1]
PROTOCOL_VERSION = "5"
PROGRESS_IDLE_DEADLINE_MS = 600_000
# The socket waits this long past the deadline the worker was told, so a worker
# that honours its own idle deadline reports it before the client gives up.
CALL_TIMEOUT_MARGIN_SECONDS = 30
VIDEO_EXTENSIONS = {
    ".avi", ".m2ts", ".m4v", ".mkv", ".mov", ".mp4", ".mpeg", ".mpg", ".ts", ".webm",
}
# Storage classes are allow-lists: an unrecognized type (autofs, a FUSE remote) is
# `unknown`, never `local`, so `--evict` refuses it.
LOCAL_FILESYSTEMS = {
    "bcachefs", "btrfs", "ext2", "ext3", "ext4", "f2fs", "jfs", "tmpfs", "xfs", "zfs",
}
NETWORK_FILESYSTEMS = {
    "9p", "afs", "ceph", "cifs", "fuse.sshfs", "glusterfs", "ncpfs", "nfs", "nfs4", "smb3",
    "smbfs",
}
# Every read the replayed sequence performs: one hash-worker pass plus the probe
# worker's pre- and post-probe observation passes.
READS_PER_FILE = 3
# Whole-file reads a row completed, by outcome; a failed operation's partial read is not
# counted here, and worker `rchar` cross-checks the total.
READS_COMPLETED = {"observed": 3, "disagree": 3, "probe_failed": 1, "hash_failed": 0}
# Source constants that define current behavior, read from the checkout so a
# record always names the values of the build it measured.
SOURCE_CONSTANTS = {
    "hash_chunk_bytes": (
        "crates/voom-hash-worker/src/hash.rs",
        r"pub const HASH_CHUNK_BYTES: usize = ([0-9_ *]+);",
    ),
    "probe_observe_buffer_bytes": (
        "crates/voom-ffprobe-worker/src/observe.rs",
        r"let mut buffer = \[0_u8; ([0-9_ *]+)\];",
    ),
    "max_in_flight": (
        "crates/voom-node-agent/src/scan_session.rs",
        r"const MAX_IN_FLIGHT: usize = ([0-9_ *]+);",
    ),
    "flush_observation_count": (
        "crates/voom-node-agent/src/scan_session.rs",
        r"const FLUSH_OBSERVATION_COUNT: usize = ([0-9_ *]+);",
    ),
    "flush_estimated_bytes": (
        "crates/voom-node-agent/src/scan_session.rs",
        r"const FLUSH_ESTIMATED_BYTES: usize = ([0-9_ *]+);",
    ),
}
NOT_MEASURED = {
    "discovery": "replay starts from a manifest; the scan worker is not run",
    "first_durable_observation": "replay does not publish batches to the control plane",
    "first_actionable_plan": "replay does not plan",
    "first_transformation_start": "replay does not transform",
    "first_verified_output": "replay does not verify outputs",
    "complete_library": "replay covers a recorded subset, not a whole library",
    "sidecar_hashing": "replay hashes primaries only",
    "ffprobe_child_peak_memory_and_io": "per-process counters exclude the ffprobe child",
    "retries": "replay performs no retries; failures are recorded as rows",
}


class HarnessError(Exception):
    """An operator-actionable refusal or capture failure."""


def read_constants(repo_root: Path) -> dict[str, int]:
    values = {}
    for name, (relative, pattern) in SOURCE_CONSTANTS.items():
        path = repo_root / relative
        try:
            text = path.read_text(encoding="utf-8")
        except OSError as error:
            raise HarnessError(f"cannot read {relative} for {name}: {error}") from error
        match = re.search(pattern, text)
        if match is None:
            raise HarnessError(f"constant {name} not found in {relative}; update the harness")
        product = 1
        for factor in match.group(1).replace("_", "").split("*"):
            product *= int(factor.strip())
        values[name] = product
    return values


def refuse_private_path_in_checkout(output: Path) -> None:
    """Private manifests hold host paths; refuse any Git worktree path Git would not ignore."""
    output = output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    inside = subprocess.run(
        ["git", "-C", str(output.parent), "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=False,
    )
    if inside.returncode != 0:
        return
    ignored = subprocess.run(
        ["git", "-C", str(output.parent), "check-ignore", "-q", str(output)], check=False
    )
    if ignored.returncode != 0:
        raise HarnessError(
            "private manifest must live outside the checkout or at a path Git ignores"
        )


def build_manifest(root: Path, label: str) -> dict[str, Any]:
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", label):
        raise HarnessError("corpus label must be lowercase letters, digits and hyphens")
    root = root.resolve()
    entries = []
    for directory, dirnames, filenames in os.walk(root):
        dirnames.sort()
        for name in filenames:
            path = Path(directory, name)
            if path.suffix.lower() not in VIDEO_EXTENSIONS or path.is_symlink():
                continue
            facts = path.stat()
            entries.append((path.relative_to(root).as_posix(), facts.st_size, facts.st_mtime_ns))
    entries.sort(key=lambda entry: entry[0].encode())
    files = [
        {"id": f"f{index:04d}", "relative": relative, "size_bytes": size,
         "mtime_ns": mtime_ns, "ext": Path(relative).suffix.lower()}
        for index, (relative, size, mtime_ns) in enumerate(entries, start=1)
    ]
    return {"label": label, "root": str(root), "files": files}


def filesystem_type(path: Path, mounts: Path = Path("/proc/mounts")) -> str:
    """Type of the effective mount: longest prefix, and the later of equal mountpoints."""
    try:
        lines = mounts.read_text(encoding="utf-8").splitlines()
    except OSError:
        return UNAVAILABLE
    resolved = path.resolve()
    best, best_type = None, UNAVAILABLE
    for line in lines:
        fields = line.split()
        point = re.sub(r"\\([0-7]{3})", lambda match: chr(int(match.group(1), 8)), fields[1])
        mount = Path(point)
        if resolved.is_relative_to(mount) and (best is None or len(str(mount)) >= len(str(best))):
            best, best_type = mount, fields[2]
    return best_type


def corpus_filesystem(root: Path, files: list[dict[str, Any]]) -> str:
    """One filesystem type for every selected file, so eviction and storage class hold."""
    types = {filesystem_type(root / entry["relative"]) for entry in files}
    if len(types) != 1:
        raise HarnessError("selected files span filesystems; select ids from one mount")
    return types.pop()


def storage_class(fs_type: str) -> str:
    if fs_type == UNAVAILABLE:
        return UNAVAILABLE
    if fs_type in LOCAL_FILESYSTEMS:
        return "local"
    return "network" if fs_type in NETWORK_FILESYSTEMS else "unknown"


def resident_bytes(path: Path) -> int | str:
    if shutil.which("fincore") is None:
        return UNAVAILABLE
    result = subprocess.run(
        ["fincore", "--bytes", "--noheadings", "--raw", "--output", "RES", str(path)],
        capture_output=True, text=True, check=False,
    )
    value = result.stdout.strip()
    return int(value) if result.returncode == 0 and value.isdigit() else UNAVAILABLE


def evict(path: Path, fs_type: str) -> None:
    """Drop clean page-cache pages for one file without changing it."""
    if storage_class(fs_type) != "local":
        raise HarnessError(f"refusing --evict on {fs_type} storage; eviction is local-only")
    flags = os.O_RDONLY | getattr(os, "O_NOATIME", 0)
    try:
        descriptor = os.open(path, flags)
    except PermissionError:
        descriptor = os.open(path, os.O_RDONLY)
    try:
        os.posix_fadvise(descriptor, 0, 0, os.POSIX_FADV_DONTNEED)
    finally:
        os.close(descriptor)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def git_output(*arguments: str, repo_root: Path = REPO_ROOT) -> str | None:
    result = subprocess.run(
        ["git", "-C", str(repo_root), *arguments], capture_output=True, text=True, check=False
    )
    return result.stdout if result.returncode == 0 else None


def tracked_changes(repo_root: Path = REPO_ROOT) -> list[str] | str:
    """Changed tracked paths only: untracked names are arbitrary and may not be public."""
    status = git_output("status", "--porcelain", "-z", "--untracked-files=no", repo_root=repo_root)
    return UNAVAILABLE if status is None else changed_paths(status)


def changed_paths(porcelain_z: str) -> list[str]:
    """Paths from `git status --porcelain -z`; a rename's source entry is skipped."""
    paths, entries = [], iter(porcelain_z.split("\0"))
    for entry in entries:
        if not entry:
            continue
        paths.append(entry[3:])
        if entry[0] in "RC":
            next(entries, None)
    return sorted(paths)


def ffprobe_version() -> str:
    binary = os.environ.get("VOOM_FFPROBE_BIN") or shutil.which("ffprobe")
    if not binary:
        return UNAVAILABLE
    result = subprocess.run([binary, "-version"], capture_output=True, text=True, check=False)
    words = result.stdout.split()
    return words[2] if result.returncode == 0 and len(words) > 2 else UNAVAILABLE


def build_identity(worker_dir: Path) -> dict[str, Any]:
    commit = git_output("rev-parse", "HEAD")
    return {
        "git_commit": commit.strip() if commit else UNAVAILABLE,
        "git_changed_paths": tracked_changes(),
        "harness_sha256": sha256_file(Path(__file__).resolve()),
        "hash_worker_sha256": sha256_file(worker_dir / "voom-hash-worker"),
        "probe_worker_sha256": sha256_file(worker_dir / "voom-ffprobe-worker"),
        "ffprobe_version": ffprobe_version(),
    }


def host_identity() -> dict[str, Any]:
    memory = UNAVAILABLE
    try:
        for line in Path("/proc/meminfo").read_text(encoding="utf-8").splitlines():
            if line.startswith("MemTotal:"):
                memory = int(line.split()[1]) * 1024
    except OSError:
        pass
    return {
        "os": platform.system(), "machine": platform.machine(), "cpu_count": os.cpu_count(),
        "memory_bytes": memory, "python": platform.python_version(),
    }


def host_counters() -> dict[str, int | str]:
    """Host-wide counters; on a shared host they include unrelated traffic."""
    counters: dict[str, int | str] = {
        "network_rx_bytes": UNAVAILABLE, "cifs_bytes_read": UNAVAILABLE,
    }
    try:
        lines = Path("/proc/net/dev").read_text(encoding="utf-8").splitlines()[2:]
        counters["network_rx_bytes"] = sum(
            int(line.split(":", 1)[1].split()[0]) for line in lines
            if line.split(":", 1)[0].strip() != "lo"
        )
    except (OSError, IndexError, ValueError):
        pass
    try:
        text = Path("/proc/fs/cifs/Stats").read_text(encoding="utf-8")
        reads = re.findall(r"Bytes read: (\d+)", text)
        counters["cifs_bytes_read"] = sum(int(value) for value in reads)
    except OSError:
        pass
    return counters


def counter_delta(before: dict[str, Any], after: dict[str, Any]) -> dict[str, int | str]:
    return {
        key: after[key] - before[key]
        if isinstance(before[key], int) and isinstance(after[key], int) else UNAVAILABLE
        for key in before
    }


class Worker:
    """One built worker process driven over its authenticated HTTP protocol."""

    def __init__(self, binary: Path):
        self.secret = secrets.token_hex(24)
        environment = dict(
            os.environ, VOOM_WORKER_SECRET=self.secret, VOOM_WORKER_ID="1", VOOM_WORKER_EPOCH="1"
        )
        self.process = subprocess.Popen(
            [str(binary)], env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True,
        )
        line = self.process.stdout.readline()
        match = re.fullmatch(r"BOUND addr=([0-9.]+):(\d+)\n", line)
        if match is None:
            self.stop()
            raise HarnessError(f"{binary.name} did not report a bound address")
        self.host, self.port = match.group(1), int(match.group(2))
        self.lease = 0
        self.lock = threading.Lock()

    def call(self, operation: str, payload: dict[str, Any]) -> dict[str, Any]:
        """Return the terminal frame, or a failure dict for any protocol surprise."""
        with self.lock:
            self.lease += 1
            lease = self.lease
        body = json.dumps({
            "operation": operation, "lease_id": lease, "payload": payload,
            "heartbeat_deadline_ms": PROGRESS_IDLE_DEADLINE_MS,
            "progress_idle_deadline_ms": PROGRESS_IDLE_DEADLINE_MS,
        })
        timeout = PROGRESS_IDLE_DEADLINE_MS / 1000 + CALL_TIMEOUT_MARGIN_SECONDS
        connection = http.client.HTTPConnection(self.host, self.port, timeout=timeout)
        try:
            connection.request("POST", "/v1/operations", body, {
                "Authorization": f"Bearer {self.secret}", "x-voom-worker-id": "1",
                "x-voom-worker-epoch": "1", "x-voom-protocol-version": PROTOCOL_VERSION,
                "x-voom-idempotency-key": secrets.token_hex(16),
                "Content-Type": "application/json",
            })
            response = connection.getresponse()
            text = response.read().decode("utf-8", "replace")
        except TimeoutError:
            return {"kind": "failure", "code": "transport_timeout"}
        except (OSError, http.client.HTTPException) as error:
            return {"kind": "failure", "code": f"transport_{type(error).__name__}"}
        finally:
            connection.close()
        if response.status != 200:
            return {"kind": "failure", "code": f"http_{response.status}"}
        return terminal_frame(text.splitlines()[1:])

    def usage(self) -> dict[str, int | float | str]:
        """CPU includes reaped children; memory and I/O are this process only."""
        proc = Path("/proc", str(self.process.pid))
        usage: dict[str, int | float | str] = {
            "cpu_seconds": UNAVAILABLE, "peak_rss_bytes": UNAVAILABLE,
            "rchar": UNAVAILABLE, "read_bytes": UNAVAILABLE,
        }
        try:
            fields = (proc / "stat").read_text().rsplit(")", 1)[1].split()
            ticks = sum(int(value) for value in fields[11:15])
            usage["cpu_seconds"] = round(ticks / os.sysconf("SC_CLK_TCK"), 3)
            for line in (proc / "status").read_text().splitlines():
                if line.startswith("VmHWM:"):
                    usage["peak_rss_bytes"] = int(line.split()[1]) * 1024
            for line in (proc / "io").read_text().splitlines():
                key, value = line.split(":")
                if key in ("rchar", "read_bytes"):
                    usage[key] = int(value)
        except (OSError, IndexError, ValueError):
            pass
        return usage

    def stop(self) -> None:
        if self.process.stdin and not self.process.stdin.closed:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.process.stdout.close()


def terminal_frame(lines: list[str]) -> dict[str, Any]:
    for line in lines:
        try:
            frame = json.loads(line)
        except ValueError:
            return {"kind": "failure", "code": "malformed_frame"}
        kind = frame.get("kind") if isinstance(frame, dict) else None
        if kind in ("result", "error"):
            return frame
        if kind != "progress":
            return {"kind": "failure", "code": "unknown_frame"}
    return {"kind": "failure", "code": "no_terminal_frame"}


# The probe worker omits `modified_at` when it has none; production then treats the
# observation as disagreeing, so only size and hash are required to be present.
OBSERVED_FIELDS = {"size_bytes", "content_hash"}
HASH_FIELDS = OBSERVED_FIELDS | {"modified_at"}
MALFORMED_RESULT = {"kind": "failure", "code": "malformed_result"}


def has_fields(value: Any, fields: set[str]) -> bool:
    return isinstance(value, dict) and fields <= value.keys()


def failure_of(stage: str, frame: dict[str, Any]) -> dict[str, Any]:
    """Keep class and code only: worker messages can carry host paths."""
    return {"stage": stage, "class": frame.get("class", "harness"), "code": frame.get("code")}


def replay_file(entry: dict[str, Any], root: Path, workers: dict[str, Worker],
                started: float) -> dict[str, Any]:
    row: dict[str, Any] = {
        "id": entry["id"], "size_bytes": entry["size_bytes"], "ext": entry["ext"],
        "queue_delay_seconds": round(time.monotonic() - started, 6),
        "hash_seconds": UNAVAILABLE, "probe_seconds": UNAVAILABLE, "failure": None,
    }
    begin = time.monotonic()
    hashed = workers["hash"].call("hash_file", {
        "provider_locator": str(root), "provider_relative_locator": entry["relative"],
    })
    row["hash_seconds"] = round(time.monotonic() - begin, 6)
    if hashed.get("kind") != "result":
        row.update(outcome="hash_failed", failure=failure_of("hash", hashed))
        return row
    if not has_fields(hashed.get("payload"), HASH_FIELDS):
        row.update(outcome="hash_failed", failure=failure_of("hash", MALFORMED_RESULT))
        return row
    facts = hashed["payload"]
    begin = time.monotonic()
    probed = workers["probe"].call("probe_file", {
        "path": str(root / entry["relative"]),
        "expected": {
            "size_bytes": facts["size_bytes"], "content_hash": facts["content_hash"],
            "modified_at": facts["modified_at"], "local_file_key": None,
        },
    })
    row["probe_seconds"] = round(time.monotonic() - begin, 6)
    if probed.get("kind") != "result":
        row.update(outcome="probe_failed", failure=failure_of("probe", probed))
        return row
    observed = probed.get("payload")
    sides = [observed.get(side) for side in ("pre_probe", "post_probe")] \
        if isinstance(observed, dict) else []
    if not sides or not all(has_fields(side, OBSERVED_FIELDS) for side in sides):
        row.update(outcome="probe_failed", failure=failure_of("probe", MALFORMED_RESULT))
        return row
    agreed = all(
        side.get(field) == facts[field] for side in sides for field in HASH_FIELDS
    )
    row["outcome"] = "observed" if agreed else "disagree"
    return row


def within_budget(files: list[dict[str, Any]], max_bytes: int) -> tuple[list, list]:
    scheduled, skipped, total = [], [], 0
    for entry in files:
        cost = entry["size_bytes"] * READS_PER_FILE
        if total + cost > max_bytes:
            skipped.append(entry["id"])
            continue
        scheduled.append(entry)
        total += cost
    return scheduled, skipped


def run_repetition(index: int, manifest: dict[str, Any], files: list[dict[str, Any]],
                   args: argparse.Namespace, fs_type: str) -> dict[str, Any]:
    root = Path(manifest["root"])
    paths = {entry["id"]: root / entry["relative"] for entry in files}
    if args.evict:
        for path in paths.values():
            evict(path, fs_type)
    residency = {file_id: resident_bytes(path) for file_id, path in paths.items()}
    workers = {
        "hash": Worker(args.worker_dir / "voom-hash-worker"),
        "probe": Worker(args.worker_dir / "voom-ffprobe-worker"),
    }
    try:
        counters = host_counters()
        started = time.monotonic()
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.concurrency) as pool:
            rows = list(pool.map(lambda entry: replay_file(entry, root, workers, started), files))
        wall = time.monotonic() - started
        usage = {name: worker.usage() for name, worker in workers.items()}
        counters = counter_delta(counters, host_counters())
    finally:
        for worker in workers.values():
            worker.stop()
    for row in rows:
        row["resident_bytes_before"] = residency[row["id"]]
    return {
        "index": index, "evicted": args.evict, "wall_seconds": round(wall, 6),
        "logical_bytes_read": sum(
            row["size_bytes"] * READS_COMPLETED[row["outcome"]] for row in rows
        ),
        "files": rows, "workers": usage, "host_counters": counters,
    }


def select_files(manifest: dict[str, Any], ids: str | None) -> list[dict[str, Any]]:
    if ids is None:
        return list(manifest["files"])
    wanted = ids.split(",")
    by_id = {entry["id"]: entry for entry in manifest["files"]}
    missing = [file_id for file_id in wanted if file_id not in by_id]
    if missing:
        raise HarnessError(f"unknown manifest ids: {','.join(missing)}")
    return [by_id[file_id] for file_id in wanted]


def capture(args: argparse.Namespace) -> dict[str, Any]:
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    constants = read_constants(REPO_ROOT)
    args.concurrency = args.concurrency or constants["max_in_flight"]
    selected = select_files(manifest, args.ids)
    fs_type = corpus_filesystem(Path(manifest["root"]), selected)
    # Identify the harness and workers before measuring, so the record names the bytes that ran.
    build = build_identity(args.worker_dir)
    files, skipped = within_budget(selected, args.max_bytes)
    if not files:
        raise HarnessError("no selected file fits --max-bytes")
    repetitions = [
        run_repetition(index, manifest, files, args, fs_type)
        for index in range(1, args.repetitions + 1)
    ]
    return {
        "schema": SCHEMA,
        "corpus": {
            "label": manifest["label"], "filesystem_type": fs_type,
            "storage_class": storage_class(fs_type), "manifest_files": len(manifest["files"]),
            "selected_ids": [entry["id"] for entry in files], "skipped_budget_ids": skipped,
        },
        "build": build,
        "host": host_identity(),
        "parameters": {
            **constants, "concurrency": args.concurrency, "reads_per_file": READS_PER_FILE,
            "max_bytes_per_repetition": args.max_bytes, "repetitions": args.repetitions,
            "evict": args.evict,
        },
        "repetitions": repetitions,
        "not_measured": NOT_MEASURED,
    }


def is_metric(value: Any) -> bool:
    number = isinstance(value, (int, float)) and not isinstance(value, bool)
    return value == UNAVAILABLE or (number and value >= 0)


REQUIRED_KEYS = {
    "corpus": {"label", "storage_class", "filesystem_type", "manifest_files", "selected_ids",
               "skipped_budget_ids"},
    "build": {"git_commit", "git_changed_paths", "harness_sha256", "hash_worker_sha256",
              "probe_worker_sha256", "ffprobe_version"},
    "host": {"os", "machine", "cpu_count", "memory_bytes", "python"},
    "parameters": {*SOURCE_CONSTANTS, "concurrency", "evict", "repetitions", "reads_per_file",
                   "max_bytes_per_repetition"},
}
REPETITION_KEYS = {"index", "evicted", "wall_seconds", "logical_bytes_read", "workers",
                   "host_counters", "files"}
WORKER_KEYS = {"cpu_seconds", "peak_rss_bytes", "rchar", "read_bytes"}
COUNTER_KEYS = {"network_rx_bytes", "cifs_bytes_read"}
ROW_KEYS = {"id", "size_bytes", "ext", "resident_bytes_before", "queue_delay_seconds",
            "hash_seconds", "probe_seconds", "outcome", "failure"}


def missing(label: str, value: Any, keys: set[str]) -> list[str]:
    present = set(value) if isinstance(value, dict) else set()
    return [f"{label} {key}" for key in sorted(keys - present)]


def row_problems(row: dict[str, Any]) -> list[str]:
    problems = missing("file", row, ROW_KEYS)
    if not re.fullmatch(r"f\d{4}", str(row.get("id", ""))):
        problems.append("file id")
    for key in ("size_bytes", "queue_delay_seconds", "hash_seconds", "probe_seconds",
                "resident_bytes_before"):
        if not is_metric(row.get(key)):
            problems.append(f"file {key}")
    outcome = row.get("outcome")
    if outcome not in READS_COMPLETED:
        problems.append("file outcome")
    failure = row.get("failure")
    failed = outcome in ("hash_failed", "probe_failed")
    if failed and (not isinstance(failure, dict) or set(failure) != {"stage", "class", "code"}):
        problems.append("file failure")
    if not failed and failure is not None:
        problems.append("file failure")
    return problems


def repetition_problems(repetition: dict[str, Any]) -> list[str]:
    problems = missing("repetition", repetition, REPETITION_KEYS)
    for key in ("wall_seconds", "logical_bytes_read"):
        if not is_metric(repetition.get(key)):
            problems.append(f"repetition {key}")
    workers = repetition.get("workers") or {}
    counters = repetition.get("host_counters") or {}
    rows = repetition.get("files") or []
    if not rows:
        problems.append("repetition files")
    for name in ("hash", "probe"):
        problems += missing(f"worker {name}", workers.get(name), WORKER_KEYS)
    for worker in workers.values():
        problems += [f"worker {key}" for key, value in worker.items() if not is_metric(value)]
    problems += missing("host_counters", counters, COUNTER_KEYS)
    problems += ["host counter" for value in counters.values() if not is_metric(value)]
    for row in rows:
        problems += row_problems(row)
    return problems


def validate_run(record: dict[str, Any]) -> list[str]:
    """Return every contract violation in a committed-safe run record."""
    problems = []
    if record.get("schema") != SCHEMA:
        problems.append("schema")
    for section in ("corpus", "build", "host", "parameters", "repetitions", "not_measured"):
        if section not in record:
            problems.append(f"missing {section}")
    if problems:
        return problems
    for section, keys in REQUIRED_KEYS.items():
        problems += missing(section, record[section], keys)
    if set(record["not_measured"]) != set(NOT_MEASURED):
        problems.append("not_measured")
    if not record["repetitions"]:
        problems.append("repetitions")
    for repetition in record["repetitions"]:
        problems += repetition_problems(repetition)
    text = json.dumps(record)
    if re.search(r'"/|\\\\|/home/|/Users/|/mnt/', text):
        problems.append("host path")
    return problems


def write_new(path: Path, value: dict[str, Any], mode: int) -> None:
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    with os.fdopen(descriptor, "w", encoding="utf-8") as output:
        json.dump(value, output, indent=2, sort_keys=True)
        output.write("\n")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    manifest = commands.add_parser("manifest", help="list a corpus into a private manifest")
    manifest.add_argument("root", type=Path)
    manifest.add_argument("--label", required=True)
    manifest.add_argument("--output", type=Path, required=True)
    run = commands.add_parser("run", help="replay current behavior over a manifest subset")
    run.add_argument("manifest", type=Path)
    run.add_argument("--worker-dir", type=Path, default=REPO_ROOT / "target" / "release")
    run.add_argument("--output", type=Path, required=True)
    run.add_argument("--ids", help="comma-separated manifest ids; default is every file")
    run.add_argument("--max-bytes", type=int, required=True,
                     help="logical-read budget per repetition (size x reads per file)")
    run.add_argument("--repetitions", type=int, default=3)
    run.add_argument("--concurrency", type=int, help="default: source MAX_IN_FLIGHT")
    run.add_argument("--evict", action="store_true",
                     help="drop page cache for selected files first (local storage only)")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        if args.command == "manifest":
            refuse_private_path_in_checkout(args.output)
            write_new(args.output, build_manifest(args.root, args.label), 0o600)
        else:
            record = capture(args)
            problems = validate_run(record)
            if problems:
                raise HarnessError("run record failed validation: " + ", ".join(problems))
            write_new(args.output, record, 0o644)
    except (HarnessError, OSError, ValueError, KeyError) as error:
        print(f"media-evidence-baseline: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
