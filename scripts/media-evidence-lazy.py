#!/usr/bin/env python3
"""Experiment-only lazy full-hash verification lifecycle for issue #797.

`matrix` drives the per-file evidence lifecycle (unknown, provisional, verified, stale,
disagreement) through a recovery matrix on scratch fixtures. `run` measures one lifecycle per
repetition over a private #788 manifest: observe every file, re-observe it twice as the probe
would, then verify it with one full read. `compare` applies the frozen protocol thresholds to
a lazy run and the matching #788 baseline record. Digests are `exp-blake2b:` values that never
leave this process; records carry states, byte counts and timings only. See
docs/workflow/specs/2026-10-29-lazy-hash-lifecycle-design.md.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import errno
import hashlib
import importlib.util
import json
import os
import re
import signal
import statistics
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Callable

_spec = importlib.util.spec_from_file_location(
    "media_evidence_baseline", Path(__file__).with_name("media-evidence-baseline.py")
)
baseline = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(baseline)

UNAVAILABLE = baseline.UNAVAILABLE
HarnessError = baseline.HarnessError
RUN_SCHEMA = "media-evidence-lazy-run/1"
MATRIX_SCHEMA = "media-evidence-lazy-matrix/1"
DIGEST_PREFIX = "exp-blake2b:"
# The fixed provisional fingerprint is an instrument, not a selection; #796 owns that search.
FINGERPRINT_WINDOW_BYTES = 64 * 1024
READ_CHUNK_BYTES = 1 << 20
MAX_VERIFY_ATTEMPTS = 2
# The owner observation plus the probe's pre- and post-probe re-observations.
OBSERVATIONS_PER_FILE = 3
FIXTURE_BYTES = 4 * READ_CHUNK_BYTES
STATES = ("unknown", "provisional", "verified", "stale", "disagreement")
NOT_MEASURED = {
    "discovery": "run starts from a manifest; the scan worker is not run",
    "first_durable_observation": "the prototype publishes nothing to the control plane",
    "first_actionable_plan": "the prototype does not plan corpus files",
    "first_transformation_start": "the prototype does not transform",
    "first_verified_output": "the prototype does not verify outputs",
    "complete_library": "run covers a recorded subset, not a whole library",
    "sidecar_hashing": "run observes primaries only",
    "ffprobe_reads": "probe re-observations read fingerprints; ffprobe itself is not run",
    "retries": "run performs no retries; a stale or failed verify is a recorded row",
}
HOST_PATH = re.compile(r'"/|\\\\|/home/|/Users/|/mnt/')


class Interrupted(Exception):
    """A verification stopped before its last read; `bytes_read` were read and wasted."""

    def __init__(self, bytes_read: int):
        super().__init__(f"interrupted after {bytes_read} bytes")
        self.bytes_read = bytes_read


class ReadFailed(Exception):
    """A verification read failed; `bytes_read` were read and wasted."""

    def __init__(self, bytes_read: int, code: str):
        super().__init__(f"read failed ({code}) after {bytes_read} bytes")
        self.bytes_read = bytes_read
        self.code = code


def stat_key(facts: os.stat_result) -> dict[str, Any]:
    return {"size": facts.st_size, "mtime_ns": facts.st_mtime_ns,
            "inode": [facts.st_dev, facts.st_ino]}


def tail_start(size: int) -> int:
    """The tail window never overlaps the head window."""
    return max(FINGERPRINT_WINDOW_BYTES, size - FINGERPRINT_WINDOW_BYTES)


def window_digest(head: bytes, tail: bytes) -> str:
    digest = hashlib.blake2b(digest_size=32)
    digest.update(head)
    digest.update(tail)
    return DIGEST_PREFIX + digest.hexdigest()


def read_fingerprint(path: Path) -> tuple[dict[str, Any], int]:
    """Stat plus head/tail digest from one read-only handle; returns bytes read."""
    with path.open("rb") as handle:
        key = stat_key(os.fstat(handle.fileno()))
        head = handle.read(FINGERPRINT_WINDOW_BYTES)
        tail = b""
        if key["size"] > FINGERPRINT_WINDOW_BYTES:
            handle.seek(tail_start(key["size"]))
            tail = handle.read(FINGERPRINT_WINDOW_BYTES)
    return {**key, "window": window_digest(head, tail)}, len(head) + len(tail)


class Hooks:
    """Deterministic fault injection for one verification attempt; the default injects none."""

    def after_chunk(self, path: Path, bytes_read: int) -> None:
        """Called after each full-read chunk with the cumulative bytes read."""


def full_pass(path: Path, size: int, hooks: Hooks) -> tuple[str, str, int]:
    """Digest and head/tail window digest of one whole read; returns bytes read."""
    digest = hashlib.blake2b(digest_size=32)
    buffer = bytearray(READ_CHUNK_BYTES)
    view = memoryview(buffer)
    head, tail, offset, tail_from = bytearray(), bytearray(), 0, tail_start(size)
    with path.open("rb") as handle:
        while True:
            try:
                count = handle.readinto(buffer)
            except OSError as error:
                raise ReadFailed(offset, errno.errorcode.get(error.errno, "EIO")) from error
            if not count:
                break
            chunk = view[:count]
            digest.update(chunk)
            if offset < FINGERPRINT_WINDOW_BYTES:
                head += chunk[:FINGERPRINT_WINDOW_BYTES - offset]
            if size > FINGERPRINT_WINDOW_BYTES and offset + count > tail_from:
                tail += chunk[max(0, tail_from - offset):]
            offset += count
            hooks.after_chunk(path, offset)
    return DIGEST_PREFIX + digest.hexdigest(), window_digest(bytes(head), bytes(tail)), offset


class StateFile:
    """Per-file records, replaced atomically after each transition."""

    def __init__(self, path: Path):
        self.path = path

    def load(self) -> dict[str, dict[str, Any]]:
        if not self.path.exists():
            return {}
        return json.loads(self.path.read_text(encoding="utf-8"))

    def save(self, records: dict[str, dict[str, Any]]) -> None:
        descriptor, temporary = tempfile.mkstemp(dir=self.path.parent, prefix=".state.")
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            json.dump(records, output, sort_keys=True)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, self.path)


class Lifecycle:
    """The evidence lifecycle for files keyed by id; every transition is persisted."""

    def __init__(self, state_file: StateFile):
        self.state_file = state_file
        self.records = state_file.load()
        # Byte reads run outside the lock; record changes and their persistence run inside it.
        self.lock = threading.Lock()

    def record(self, file_id: str) -> dict[str, Any]:
        return self.records.setdefault(file_id, {
            "state": "unknown", "fingerprint": None, "digest": None, "plans": {}, "outputs": {},
        })

    def state(self, file_id: str) -> str:
        return self.record(file_id)["state"]

    def _invalidate(self, record: dict[str, Any], state: str) -> None:
        record.update(state=state, digest=None)
        for plan in record["plans"]:
            record["plans"][plan] = "revoked"
        for output in record["outputs"]:
            record["outputs"][output] = "quarantined"

    def observe(self, file_id: str, path: Path) -> int:
        """Fingerprint a file; changed provisional or verified evidence goes stale."""
        fingerprint, bytes_read = read_fingerprint(path)
        with self.lock:
            record = self.record(file_id)
            if record["state"] in ("provisional", "verified"):
                if fingerprint != record["fingerprint"]:
                    self._invalidate(record, "stale")
            else:
                record.update(state="provisional", fingerprint=fingerprint, digest=None)
            self.state_file.save(self.records)
        return bytes_read

    def plan(self, file_id: str, plan_id: str) -> None:
        with self.lock:
            record = self.record(file_id)
            if record["state"] not in ("provisional", "verified"):
                raise HarnessError(f"cannot plan on {record['state']} evidence")
            record["plans"][plan_id] = "committable" if record["state"] == "verified" else "held"
            self.state_file.save(self.records)

    def produce(self, file_id: str, output_id: str) -> None:
        with self.lock:
            self.record(file_id)["outputs"][output_id] = "kept"
            self.state_file.save(self.records)

    def _finish(self, record: dict[str, Any], state: str, digest: str | None = None) -> None:
        with self.lock:
            if state == "verified":
                record.update(state=state, digest=digest)
                # Revocation is terminal: a plan made on earlier bytes never becomes committable.
                for plan, disposition in record["plans"].items():
                    if disposition == "held":
                        record["plans"][plan] = "committable"
            else:
                self._invalidate(record, state)
            self.state_file.save(self.records)

    def verify(self, file_id: str, path: Path, hooks: Hooks | None = None) -> tuple[str, int]:
        """One full read between two stats. Interruption raises and leaves the state file as is."""
        with self.lock:
            record = self.record(file_id)
            if record["state"] != "provisional":
                return "refused", 0
            expected = dict(record["fingerprint"])
        start = stat_key(path.stat())
        if start != {key: expected[key] for key in start}:
            self._finish(record, "stale")
            return "stale", 0
        digest, window, bytes_read = full_pass(path, start["size"], hooks or Hooks())
        if stat_key(path.stat()) != start:
            self._finish(record, "stale")
        elif window != expected["window"]:
            self._finish(record, "disagreement")
        else:
            self._finish(record, "verified", digest)
        return record["state"], bytes_read


# ---------------------------------------------------------------------------- matrix


def write_fixture(path: Path, seed: int) -> None:
    path.write_bytes(hashlib.shake_256(seed.to_bytes(4, "big")).digest(FIXTURE_BYTES))


def oracle_digest(path: Path) -> str:
    """Ground truth for the oracle only; its reads are not counted as lifecycle I/O."""
    return hashlib.sha256(path.read_bytes()).hexdigest()


def overwrite_keeping_mtime(path: Path, offset: int, data: bytes) -> None:
    """Same-size edit whose stat (size, mtime ns, inode) is unchanged afterward."""
    before = path.stat()
    with path.open("r+b") as handle:
        handle.seek(offset)
        handle.write(data)
    os.utime(path, ns=(before.st_atime_ns, before.st_mtime_ns))


HEAD_EDIT = 100
INTERIOR_EDIT = 3 * READ_CHUNK_BYTES - 4096


class OnChunk(Hooks):
    """Run `action` once, after the first chunk that reaches `at_bytes`."""

    def __init__(self, at_bytes: int, action: Callable[[Path, int], None]):
        self.at_bytes, self.action, self.fired = at_bytes, action, False

    def after_chunk(self, path: Path, bytes_read: int) -> None:
        if not self.fired and bytes_read >= self.at_bytes:
            self.fired = True
            self.action(path, bytes_read)


def cancel(_path: Path, bytes_read: int) -> None:
    raise Interrupted(bytes_read)


def fail_read(_path: Path, bytes_read: int) -> None:
    raise ReadFailed(bytes_read, "EIO")


def append_byte(path: Path, _bytes_read: int) -> None:
    with path.open("ab") as handle:
        handle.write(b"\0")


def edit_interior(path: Path, _bytes_read: int) -> None:
    overwrite_keeping_mtime(path, INTERIOR_EDIT, b"\xff" * 64)


class Case:
    """One matrix case on its own fixture, with byte accounting and the oracle's truth."""

    def __init__(self, base: Path, name: str, seed: int):
        self.directory = base / name
        self.directory.mkdir()
        self.path = self.directory / "fixture.bin"
        write_fixture(self.path, seed)
        self.state_file = StateFile(self.directory / "state.json")
        self.lifecycle = Lifecycle(self.state_file)
        self.bytes = {"fingerprint": 0, "full": 0, "wasted": 0}
        self.truth = oracle_digest(self.path)

    def observe(self, record_truth: bool = True) -> None:
        """Fingerprint; the oracle's truth becomes the observed content unless told otherwise."""
        self.bytes["fingerprint"] += self.lifecycle.observe("f", self.path)
        if record_truth:
            self.truth = oracle_digest(self.path)

    def verify(self, hooks: Hooks | None = None) -> str:
        try:
            state, bytes_read = self.lifecycle.verify("f", self.path, hooks)
        except (Interrupted, ReadFailed) as error:
            self.bytes["wasted"] += error.bytes_read
            return "interrupted"
        self.bytes["full" if state == "verified" else "wasted"] += bytes_read
        return state

    def settle(self, hooks: list[Hooks]) -> None:
        """Up to MAX_VERIFY_ATTEMPTS verifications, re-observing after a stale outcome."""
        for attempt in range(MAX_VERIFY_ATTEMPTS):
            outcome = self.verify(hooks[attempt] if attempt < len(hooks) else None)
            if outcome == "stale":
                self.observe()
            elif outcome != "interrupted":
                return

    def result(self, expected: dict[str, str], **extra: Any) -> dict[str, Any]:
        record = self.lifecycle.record("f")
        changed = oracle_digest(self.path) != self.truth
        detected = record["state"] in ("stale", "disagreement")
        observed = {
            "state": record["state"],
            "oracle": ("detected" if detected else "missed") if changed else "no-change",
            "plan": record["plans"].get("p", "none"),
            "output": record["outputs"].get("o", "none"),
            "digest_published": record["digest"] is not None,
            **extra,
        }
        return {"expected": expected, "observed": observed, "bytes": dict(self.bytes)}


def expect(state: str, oracle: str, plan: str, output: str = "kept") -> dict[str, Any]:
    return {"state": state, "oracle": oracle, "plan": plan, "output": output,
            "digest_published": state == "verified"}


def planned(case: Case) -> Case:
    case.observe()
    case.lifecycle.plan("f", "p")
    case.lifecycle.produce("f", "o")
    return case


def case_clean(case: Case) -> dict[str, Any]:
    planned(case).verify()
    return case.result(expect("verified", "no-change", "committable"))


def case_restart(case: Case) -> dict[str, Any]:
    """A child verifier SIGKILLs itself mid-read; a fresh reader loads the state file."""
    planned(case)
    reader, writer = os.pipe()
    child = os.fork()
    if child == 0:
        try:
            os.close(reader)

            def die(_path: Path, bytes_read: int) -> None:
                os.write(writer, str(bytes_read).encode())
                os.kill(os.getpid(), signal.SIGKILL)

            case.lifecycle.verify("f", case.path, OnChunk(FIXTURE_BYTES // 2, die))
        finally:
            os._exit(1)
    os.close(writer)
    with os.fdopen(reader) as progress:
        case.bytes["wasted"] += int(progress.read() or 0)
    _, status = os.waitpid(child, 0)
    killed = os.WIFSIGNALED(status) and os.WTERMSIG(status) == signal.SIGKILL
    case.lifecycle = Lifecycle(case.state_file)
    reloaded = case.result(expect("provisional", "no-change", "held"))
    case.verify()
    resumed = case.lifecycle.record("f")
    reloaded["observed"]["killed_mid_read"] = killed
    reloaded["expected"]["killed_mid_read"] = True
    reloaded["observed"]["resumed_state"] = resumed["state"]
    reloaded["expected"]["resumed_state"] = "verified"
    reloaded["bytes"] = dict(case.bytes)
    return reloaded


def case_cancel(case: Case) -> dict[str, Any]:
    planned(case).verify(OnChunk(READ_CHUNK_BYTES, cancel))
    return case.result(expect("provisional", "no-change", "held"))


def case_retry(case: Case) -> dict[str, Any]:
    planned(case).settle([OnChunk(2 * READ_CHUNK_BYTES, fail_read)])
    return case.result(expect("verified", "no-change", "committable"))


def case_retry_exhausted(case: Case) -> dict[str, Any]:
    planned(case).settle([OnChunk(READ_CHUNK_BYTES, fail_read)
                          for _ in range(MAX_VERIFY_ATTEMPTS)])
    return case.result(expect("provisional", "no-change", "held"))


def case_mutation_during_read(case: Case) -> dict[str, Any]:
    planned(case).verify(OnChunk(READ_CHUNK_BYTES, append_byte))
    return case.result(expect("stale", "detected", "revoked", "quarantined"))


def case_unseen_interior_edit_during_read(case: Case) -> dict[str, Any]:
    planned(case).verify(OnChunk(READ_CHUNK_BYTES, edit_interior))
    return case.result(expect("verified", "missed", "committable"))


def case_late_head_edit(case: Case) -> dict[str, Any]:
    planned(case)
    overwrite_keeping_mtime(case.path, HEAD_EDIT, b"\xff" * 64)
    case.verify()
    return case.result(expect("disagreement", "detected", "revoked", "quarantined"))


def case_late_interior_edit(case: Case) -> dict[str, Any]:
    planned(case)
    overwrite_keeping_mtime(case.path, INTERIOR_EDIT, b"\xff" * 64)
    case.verify()
    return case.result(expect("verified", "missed", "committable"))


def verified_then(case: Case, offset: int) -> Case:
    """Verified evidence, then a stat-invisible edit, then a re-scan observation."""
    planned(case).verify()
    overwrite_keeping_mtime(case.path, offset, b"\xff" * 64)
    case.observe(record_truth=False)
    return case


def case_stale_head_edit(case: Case) -> dict[str, Any]:
    verified_then(case, HEAD_EDIT)
    return case.result(expect("stale", "detected", "revoked", "quarantined"))


def case_stale_interior_edit(case: Case) -> dict[str, Any]:
    verified_then(case, INTERIOR_EDIT)
    return case.result(expect("verified", "missed", "committable"))


MATRIX = [
    ("clean", None, case_clean),
    ("restart", "O14", case_restart),
    ("cancel", None, case_cancel),
    ("retry", None, case_retry),
    ("retry-exhausted", None, case_retry_exhausted),
    ("concurrent-mutation", "O10", case_mutation_during_read),
    ("unseen-interior-edit-during-read", "O10", case_unseen_interior_edit_during_read),
    ("late-disagreement-head", "O15", case_late_head_edit),
    ("late-disagreement-interior", "O15", case_late_interior_edit),
    ("stale-evidence-head", "O13", case_stale_head_edit),
    ("stale-evidence-interior", "O13", case_stale_interior_edit),
]
STARVATION_BACKGROUND = 8
STARVATION_TICKS = 32
STARVATION_AGING_EVERY = 4


def starvation(policy: str) -> dict[str, Any]:
    """One verify per tick; a new on-demand file arrives every tick."""
    background = [f"b{index}" for index in range(STARVATION_BACKGROUND)]
    demand: list[str] = []
    verified_background = 0
    for tick in range(1, STARVATION_TICKS + 1):
        demand.append(f"d{tick}")
        aged_turn = policy == "aged" and tick % STARVATION_AGING_EVERY == 0
        if aged_turn and background:
            background.pop(0)
            verified_background += 1
        else:
            demand.pop(0)
    return {"policy": policy, "background_verified": verified_background,
            "background_provisional": len(background), "demand_backlog": len(demand)}


STARVATION_EXPECTED = {
    "on-demand-only": {"background_verified": 0, "background_provisional": 8},
    "aged": {"background_verified": 8, "background_provisional": 0},
}


def run_matrix() -> dict[str, Any]:
    cases = []
    with tempfile.TemporaryDirectory(prefix="lazy-matrix-") as scratch:
        for seed, (name, oracle_case, function) in enumerate(MATRIX, start=1):
            result = function(Case(Path(scratch), name, seed))
            cases.append({"case": name, "oracle_case": oracle_case, **result})
    return {
        "cases": cases,
        "starvation": [starvation(policy) for policy in STARVATION_EXPECTED],
    }


# ---------------------------------------------------------------------------- run


def fingerprint_cost(size: int) -> int:
    return min(size, FINGERPRINT_WINDOW_BYTES) + max(0, min(FINGERPRINT_WINDOW_BYTES,
                                                           size - FINGERPRINT_WINDOW_BYTES))


def lazy_cost(entry: dict[str, Any]) -> int:
    return entry["size_bytes"] + OBSERVATIONS_PER_FILE * fingerprint_cost(entry["size_bytes"])


def within_budget(files: list[dict[str, Any]], max_bytes: int) -> tuple[list, list]:
    scheduled, skipped, total = [], [], 0
    for entry in files:
        if total + lazy_cost(entry) > max_bytes:
            skipped.append(entry["id"])
            continue
        scheduled.append(entry)
        total += lazy_cost(entry)
    return scheduled, skipped


def process_usage() -> dict[str, int | float | str]:
    """This process's CPU (all threads), lifetime peak RSS and I/O counters."""
    times = os.times()
    usage: dict[str, int | float | str] = {
        "cpu_seconds": round(times.user + times.system, 3), "peak_rss_bytes": UNAVAILABLE,
        "rchar": UNAVAILABLE, "read_bytes": UNAVAILABLE,
    }
    try:
        for line in Path("/proc/self/status").read_text().splitlines():
            if line.startswith("VmHWM:"):
                usage["peak_rss_bytes"] = int(line.split()[1]) * 1024
        for line in Path("/proc/self/io").read_text().splitlines():
            key, value = line.split(":")
            if key in ("rchar", "read_bytes"):
                usage[key] = int(value)
    except (OSError, ValueError):
        pass
    return usage


def usage_delta(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
    """Counter deltas; CPU seconds are a float, and peak RSS is the process-lifetime peak."""
    delta = baseline.counter_delta(before, after)
    delta["cpu_seconds"] = round(after["cpu_seconds"] - before["cpu_seconds"], 3)
    delta["peak_rss_bytes"] = after["peak_rss_bytes"]
    return delta


def observe_row(lifecycle: Lifecycle, entry: dict[str, Any], path: Path,
                started: float) -> dict[str, Any]:
    begin = time.monotonic()
    row = {"id": entry["id"], "size_bytes": entry["size_bytes"], "ext": entry["ext"],
           "observe_delay_seconds": round(begin - started, 6), "failure": None,
           "fingerprint_bytes": 0, "probe_reobservation_bytes": 0}
    try:
        row["fingerprint_bytes"] = lifecycle.observe(entry["id"], path)
        row["observe_seconds"] = round(time.monotonic() - begin, 6)
        for _ in range(OBSERVATIONS_PER_FILE - 1):
            row["probe_reobservation_bytes"] += lifecycle.observe(entry["id"], path)
    except OSError as error:
        row["failure"] = {"stage": "observe", "class": "io",
                          "code": errno.errorcode.get(error.errno, "EIO")}
        row.setdefault("observe_seconds", UNAVAILABLE)
    return row


def verify_row(lifecycle: Lifecycle, row: dict[str, Any], path: Path,
               started: float) -> dict[str, Any]:
    begin = time.monotonic()
    row.update(verify_delay_seconds=round(begin - started, 6), full_hash_bytes=0,
               wasted_bytes=0, verify_seconds=UNAVAILABLE)
    if row["failure"] is not None:
        return row
    try:
        state, bytes_read = lifecycle.verify(row["id"], path)
    except ReadFailed as error:
        row.update(wasted_bytes=error.bytes_read,
                   failure={"stage": "verify", "class": "io", "code": error.code})
        return row
    except OSError as error:
        row["failure"] = {"stage": "verify", "class": "io",
                          "code": errno.errorcode.get(error.errno, "EIO")}
        return row
    row["full_hash_bytes" if state == "verified" else "wasted_bytes"] = bytes_read
    row["verify_seconds"] = round(time.monotonic() - begin, 6)
    return row


def run_repetition(index: int, root: Path, files: list[dict[str, Any]],
                   args: argparse.Namespace, fs_type: str) -> dict[str, Any]:
    paths = {entry["id"]: root / entry["relative"] for entry in files}
    if args.evict:
        for path in paths.values():
            baseline.evict(path, fs_type)
    residency = {file_id: baseline.resident_bytes(path) for file_id, path in paths.items()}
    with tempfile.TemporaryDirectory(prefix="lazy-run-") as scratch:
        lifecycle = Lifecycle(StateFile(Path(scratch) / "state.json"))
        usage, counters = process_usage(), baseline.host_counters()
        started = time.monotonic()
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.concurrency) as pool:
            rows = list(pool.map(
                lambda entry: observe_row(lifecycle, entry, paths[entry["id"]], started), files
            ))
            provisional = time.monotonic() - started
            rows = list(pool.map(
                lambda row: verify_row(lifecycle, row, paths[row["id"]], started), rows
            ))
        wall = time.monotonic() - started
        usage = usage_delta(usage, process_usage())
        counters = baseline.counter_delta(counters, baseline.host_counters())
        for row in rows:
            row.update(state=lifecycle.state(row["id"]), resident_bytes_before=residency[row["id"]])
    return {
        "index": index, "evicted": args.evict, "wall_seconds": round(wall, 6),
        "all_provisional_seconds": round(provisional, 6),
        "logical_bytes": logical_bytes(rows), "process": usage, "host_counters": counters,
        "files": rows,
    }


def logical_bytes(rows: list[dict[str, Any]]) -> dict[str, int]:
    components = {
        "owner_fingerprint": sum(row["fingerprint_bytes"] for row in rows),
        "probe_reobservation": sum(row["probe_reobservation_bytes"] for row in rows),
        "deferred_full_hash": sum(row["full_hash_bytes"] for row in rows),
        "wasted": sum(row["wasted_bytes"] for row in rows),
    }
    return {**components, "total": sum(components.values())}


def corpus_facts(root: Path, files: list[dict[str, Any]]) -> dict[str, list]:
    return {entry["id"]: [stat_key((root / entry["relative"]).stat())[key]
                          for key in ("size", "mtime_ns")] for entry in files}


def build_identity() -> dict[str, Any]:
    commit = baseline.git_output("rev-parse", "HEAD")
    return {
        "git_commit": commit.strip() if commit else UNAVAILABLE,
        "git_changed_paths": baseline.tracked_changes(),
        "harness_sha256": baseline.sha256_file(Path(__file__).resolve()),
        "baseline_harness_sha256": baseline.sha256_file(Path(_spec.origin).resolve()),
    }


def instrument_parameters() -> dict[str, int]:
    return {"fingerprint_window_bytes": FINGERPRINT_WINDOW_BYTES,
            "read_chunk_bytes": READ_CHUNK_BYTES, "max_verify_attempts": MAX_VERIFY_ATTEMPTS,
            "observations_per_file": OBSERVATIONS_PER_FILE}


def capture(args: argparse.Namespace) -> dict[str, Any]:
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    root = Path(manifest["root"])
    args.concurrency = args.concurrency or baseline.read_constants(baseline.REPO_ROOT)[
        "max_in_flight"]
    selected = baseline.select_files(manifest, args.ids)
    fs_type = baseline.corpus_filesystem(root, selected)
    build = build_identity()
    files, skipped = within_budget(selected, args.max_bytes)
    if not files:
        raise HarnessError("no selected file fits --max-bytes")
    before = corpus_facts(root, files)
    repetitions = [run_repetition(index, root, files, args, fs_type)
                   for index in range(1, args.repetitions + 1)]
    return {
        "schema": RUN_SCHEMA,
        "corpus": {
            "label": manifest["label"], "filesystem_type": fs_type,
            "storage_class": baseline.storage_class(fs_type),
            "manifest_files": len(manifest["files"]),
            "selected_ids": [entry["id"] for entry in files], "skipped_budget_ids": skipped,
            "unchanged_after_run": corpus_facts(root, files) == before,
        },
        "build": build,
        "host": baseline.host_identity(),
        "parameters": {**instrument_parameters(), "concurrency": args.concurrency,
                       "max_bytes_per_repetition": args.max_bytes,
                       "repetitions": args.repetitions, "evict": args.evict},
        "repetitions": repetitions,
        "not_measured": NOT_MEASURED,
    }


def matrix_record() -> dict[str, Any]:
    return {"schema": MATRIX_SCHEMA, "build": build_identity(), "host": baseline.host_identity(),
            "parameters": {**instrument_parameters(), "fixture_bytes": FIXTURE_BYTES},
            **run_matrix()}


# ---------------------------------------------------------------------------- validation


def common_problems(record: dict[str, Any]) -> list[str]:
    text = json.dumps(record)
    problems = ["host path"] if HOST_PATH.search(text) else []
    if "blake3:" in text or DIGEST_PREFIX in text:
        problems.append("digest recorded")
    for section in ("build", "host", "parameters"):
        if not isinstance(record.get(section), dict):
            problems.append(f"missing {section}")
    return problems


RUN_ROW_KEYS = {"id", "size_bytes", "ext", "resident_bytes_before", "observe_delay_seconds",
                "observe_seconds", "verify_delay_seconds", "verify_seconds",
                "fingerprint_bytes", "probe_reobservation_bytes", "full_hash_bytes",
                "wasted_bytes", "state", "failure"}
RUN_REPETITION_KEYS = {"index", "evicted", "wall_seconds", "all_provisional_seconds",
                       "logical_bytes", "process", "host_counters", "files"}
LOGICAL_KEYS = {"owner_fingerprint", "probe_reobservation", "deferred_full_hash", "wasted",
                "total"}
PROCESS_KEYS = {"cpu_seconds", "peak_rss_bytes", "rchar", "read_bytes"}
CORPUS_KEYS = {"label", "filesystem_type", "storage_class", "manifest_files", "selected_ids",
               "skipped_budget_ids", "unchanged_after_run"}


def run_row_problems(row: dict[str, Any]) -> list[str]:
    problems = baseline.missing("file", row, RUN_ROW_KEYS)
    for key in RUN_ROW_KEYS - {"id", "ext", "state", "failure"}:
        if key in row and not baseline.is_metric(row[key]):
            problems.append(f"file {key}")
    if row.get("state") not in STATES:
        problems.append("file state")
    failure = row.get("failure")
    if failure is not None and (not isinstance(failure, dict)
                                or set(failure) != {"stage", "class", "code"}):
        problems.append("file failure")
    return problems


def run_repetition_problems(repetition: dict[str, Any]) -> list[str]:
    problems = baseline.missing("repetition", repetition, RUN_REPETITION_KEYS)
    logical = repetition.get("logical_bytes") or {}
    process = repetition.get("process") or {}
    counters = repetition.get("host_counters") or {}
    problems += baseline.missing("logical_bytes", logical, LOGICAL_KEYS)
    problems += baseline.missing("process", process, PROCESS_KEYS)
    problems += baseline.missing("host_counters", counters, baseline.COUNTER_KEYS)
    values = [repetition.get("wall_seconds"), repetition.get("all_provisional_seconds"),
              *logical.values(), *process.values(), *counters.values()]
    problems += ["repetition metric" for value in values if not baseline.is_metric(value)]
    rows = repetition.get("files") or []
    if not rows:
        problems.append("repetition files")
    for row in rows:
        problems += run_row_problems(row)
    return problems


def validate_run(record: dict[str, Any]) -> list[str]:
    """Every contract violation in a committed-safe lazy run record."""
    if record.get("schema") != RUN_SCHEMA:
        return ["schema"]
    problems = common_problems(record)
    problems += baseline.missing("corpus", record.get("corpus"), CORPUS_KEYS)
    if (record.get("corpus") or {}).get("unchanged_after_run") is not True:
        problems.append("corpus changed")
    if set(record.get("not_measured") or {}) != set(NOT_MEASURED):
        problems.append("not_measured")
    if not record.get("repetitions"):
        problems.append("repetitions")
    for repetition in record.get("repetitions") or []:
        problems += run_repetition_problems(repetition)
    return problems


def validate_matrix(record: dict[str, Any]) -> list[str]:
    """Every contract violation in a matrix record, including any unmet expectation."""
    if record.get("schema") != MATRIX_SCHEMA:
        return ["schema"]
    problems = common_problems(record)
    names = [case.get("case") for case in record.get("cases") or []]
    if names != [name for name, _, _ in MATRIX]:
        problems.append("cases")
    for case in record.get("cases") or []:
        if case.get("expected") != case.get("observed"):
            problems.append(f"case {case.get('case')} expectation")
        counted = case.get("bytes")
        if not isinstance(counted, dict) or not all(map(baseline.is_metric, counted.values())):
            problems.append(f"case {case.get('case')} bytes")
    by_policy = {entry.get("policy"): entry for entry in record.get("starvation") or []}
    for policy, expected in STARVATION_EXPECTED.items():
        observed = by_policy.get(policy) or {}
        if any(observed.get(key) != value for key, value in expected.items()):
            problems.append(f"starvation {policy}")
    return problems


def validate(record: dict[str, Any]) -> list[str]:
    if record.get("schema") == MATRIX_SCHEMA:
        return validate_matrix(record)
    return validate_run(record)


# ---------------------------------------------------------------------------- compare


def comparable(lazy: dict[str, Any], base: dict[str, Any]) -> None:
    for key in ("label", "manifest_files", "selected_ids", "storage_class"):
        if lazy["corpus"].get(key) != base["corpus"].get(key):
            raise HarnessError(f"records do not compare: corpus {key} differs")
    if lazy["parameters"]["evict"] != base["parameters"]["evict"]:
        raise HarnessError("records do not compare: cache state differs")


def baseline_physical(repetition: dict[str, Any], network: bool) -> int | str:
    if network:
        return repetition["host_counters"]["cifs_bytes_read"]
    values = [worker["read_bytes"] for worker in repetition["workers"].values()]
    return sum(values) if all(isinstance(value, int) for value in values) else UNAVAILABLE


def lazy_physical(repetition: dict[str, Any], network: bool) -> int | str:
    if network:
        return repetition["host_counters"]["cifs_bytes_read"]
    return repetition["process"]["read_bytes"]


def verdict(candidate: list[float], reference: list[float]) -> dict[str, Any]:
    """Protocol thresholds for a lower-is-better metric."""
    base_median = statistics.median(reference)
    if base_median == 0 or statistics.mean(reference) == 0:
        return {"verdict": "inconclusive", "reason": "zero baseline"}
    cv = statistics.stdev(reference) / statistics.mean(reference) if len(reference) > 1 else 0.0
    median = statistics.median(candidate)
    change = (median - base_median) / base_median
    result = {"baseline_median": base_median, "candidate_median": median,
              "change": round(change, 4), "baseline_cv": round(cv, 4)}
    if cv > 0.20:
        return {**result, "verdict": "inconclusive"}
    if -change >= max(0.10, 2 * cv) and max(candidate) <= base_median:
        return {**result, "verdict": "improvement"}
    if change >= max(0.05, 2 * cv):
        return {**result, "verdict": "regression"}
    return {**result, "verdict": "inconclusive"}


def compare(lazy: dict[str, Any], base: dict[str, Any]) -> dict[str, Any]:
    comparable(lazy, base)
    network = lazy["corpus"]["storage_class"] == "network"
    first = 0 if lazy["parameters"]["evict"] else 1
    lazy_reps, base_reps = lazy["repetitions"][first:], base["repetitions"][first:]
    series = {
        "logical_bytes": ([rep["logical_bytes"]["total"] for rep in lazy_reps],
                          [rep["logical_bytes_read"] for rep in base_reps]),
        "physical_bytes": ([lazy_physical(rep, network) for rep in lazy_reps],
                           [baseline_physical(rep, network) for rep in base_reps]),
    }
    metrics, repetition_1 = {}, {}
    if first == 1:
        repetition_1 = {
            "logical_bytes": {"candidate": lazy["repetitions"][0]["logical_bytes"]["total"],
                              "baseline": base["repetitions"][0]["logical_bytes_read"]},
            "physical_bytes": {"candidate": lazy_physical(lazy["repetitions"][0], network),
                               "baseline": baseline_physical(base["repetitions"][0], network)},
        }
    for name, (candidate, reference) in series.items():
        if UNAVAILABLE in candidate or UNAVAILABLE in reference or not candidate:
            metrics[name] = {"verdict": "inconclusive", "reason": UNAVAILABLE}
        else:
            metrics[name] = verdict(candidate, reference)
    return {"corpus": lazy["corpus"]["label"], "repetitions_compared": len(lazy_reps),
            "repetition_1": repetition_1 or None, "metrics": metrics}


# ---------------------------------------------------------------------------- CLI


def read_record(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    matrix = commands.add_parser("matrix", help="run the recovery matrix on scratch fixtures")
    matrix.add_argument("--output", type=Path, required=True)
    run = commands.add_parser("run", help="measure eventual lazy I/O over a manifest subset")
    run.add_argument("manifest", type=Path)
    run.add_argument("--output", type=Path, required=True)
    run.add_argument("--ids", help="comma-separated manifest ids; default is every file")
    run.add_argument("--max-bytes", type=int, required=True,
                     help="logical-read budget per repetition")
    run.add_argument("--repetitions", type=int, default=3)
    run.add_argument("--concurrency", type=int, help="default: source MAX_IN_FLIGHT")
    run.add_argument("--evict", action="store_true",
                     help="drop page cache for selected files first (local storage only)")
    comparison = commands.add_parser("compare", help="apply protocol thresholds to two records")
    comparison.add_argument("lazy", type=Path)
    comparison.add_argument("baseline", type=Path)
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        if args.command == "compare":
            print(json.dumps(compare(read_record(args.lazy), read_record(args.baseline)),
                             indent=2, sort_keys=True))
            return 0
        record = matrix_record() if args.command == "matrix" else capture(args)
        problems = validate(record)
        if problems:
            raise HarnessError("record failed validation: " + ", ".join(problems))
        baseline.write_new(args.output, record, 0o644)
    except (HarnessError, OSError, ValueError, KeyError) as error:
        print(f"media-evidence-lazy: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
