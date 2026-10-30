#!/usr/bin/env python3
"""Rollout instrument for #794: publishing scans and constructed guard cases.

`scan` runs a real local control plane, owner node agent and bundled workers through
`acceptance-bootstrap.py` (#339's stack), requests one scan session per root, and times each
repetition from the request to its first durable observation: the first live rooted file
location the completion transaction publishes (ADR 0077). It records worker CPU, memory and
I/O, the stored evidence's hash sources, and a digest of the published content hashes, so
repeat scans compare without committing hashes. `construct` drives the release hash and
probe workers on a scratch copy to build the ADR 0120 guard cases on the filesystem that
holds the copy. Records name files by manifest id only; host paths stay in private
manifests and stack work directories outside the checkout.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import re
import sqlite3
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any, Callable

SCRIPTS = Path(__file__).resolve().parent


def load(name: str, filename: str) -> Any:
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


baseline = load("media_evidence_baseline", "media-evidence-baseline.py")
bootstrap = load("acceptance_bootstrap", "acceptance-bootstrap.py")

UNAVAILABLE = baseline.UNAVAILABLE
HarnessError = baseline.HarnessError
SCAN_SCHEMA = "media-evidence-rollout-scan/1"
CONSTRUCT_SCHEMA = "media-evidence-rollout-construct/1"
POLL_SECONDS = 0.05
WORKER_PROGRAMS = {"hash": "voom-hash-worker", "probe": "voom-ffprobe-worker"}
BINARIES = ["voom", "voom-api", "voom-node-agent", "voom-scan-worker", *WORKER_PROGRAMS.values()]
EDIT_BYTES = 4096
MISMATCH = "ARTIFACT_CHECKSUM_MISMATCH"
NOT_MEASURED = {
    "discovery": "the scan worker's enumeration is not separated from the session",
    "first_actionable_plan": "no policy is applied by a scan cell",
    "first_transformation_start": "no policy is applied by a scan cell",
    "first_verified_output": "no policy is applied by a scan cell",
    "per_file_stage_seconds": "the stack does not expose per-file hash and probe timings",
    "retries": "the stack does not expose per-operation retries",
    "ffprobe_child": "ffprobe child memory and I/O are not attributed to the probe worker",
}


class Stack(bootstrap.Bootstrap):
    """The #339 acceptance stack with the owner-node reuse setting written explicitly."""

    def __init__(self, args: argparse.Namespace, reuse: bool):
        self.reuse = reuse
        super().__init__(args)

    def configuration(self, credential: Path, base_url: str) -> str:
        setting = f"probe_hash_reuse = {'true' if self.reuse else 'false'}\n"
        return setting + super().configuration(credential, base_url)


def scan_roots(manifest: dict[str, Any], files: list[dict[str, Any]]) -> list[Path]:
    """The corpus root for a full pass; otherwise each selected file's own directory."""
    root = Path(manifest["root"])
    if {entry["id"] for entry in files} == {entry["id"] for entry in manifest["files"]}:
        return [root]
    roots = []
    for entry in files:
        parent = (root / entry["relative"]).parent
        sharing = [other["id"] for other in manifest["files"]
                   if other is not entry and (root / other["relative"]).is_relative_to(parent)]
        if parent == root or sharing:
            raise HarnessError(f"{entry['id']} shares its directory with other corpus files; "
                               "a subset root must hold exactly one manifest file")
        roots.append(parent)
    return roots


def descendants(pid: int) -> list[int]:
    parents: dict[int, list[int]] = {}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            ppid = int((entry / "stat").read_text().rsplit(")", 1)[1].split()[1])
        except (OSError, IndexError, ValueError):
            continue
        parents.setdefault(ppid, []).append(int(entry.name))
    found, pending = [], [pid]
    while pending:
        children = parents.get(pending.pop(), [])
        found += children
        pending += children
    return found


def worker_pids(agent_pid: int) -> dict[str, int]:
    """The single live hash and probe worker the owner agent supervises."""
    pids: dict[str, list[int]] = {name: [] for name in WORKER_PROGRAMS}
    for pid in descendants(agent_pid):
        try:
            program = Path(os.readlink(f"/proc/{pid}/exe")).name
        except OSError:
            continue
        for name, expected in WORKER_PROGRAMS.items():
            if program == expected:
                pids[name].append(pid)
    if any(len(found) != 1 for found in pids.values()):
        raise HarnessError("expected exactly one supervised hash and one probe worker")
    return {name: found[0] for name, found in pids.items()}


def process_usage(pid: int) -> dict[str, int | float | str]:
    """CPU includes reaped children such as ffprobe; memory and I/O are this process only."""
    usage: dict[str, int | float | str] = dict.fromkeys(baseline.WORKER_KEYS, UNAVAILABLE)
    proc = Path("/proc", str(pid))
    try:
        fields = (proc / "stat").read_text().rsplit(")", 1)[1].split()
        usage["cpu_seconds"] = sum(int(value) for value in fields[11:15]) / os.sysconf(
            "SC_CLK_TCK")
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


def usage_delta(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
    """Counters over the scan; peak memory is the process high-water mark at its end."""
    delta = {}
    for key, value in after.items():
        if UNAVAILABLE in (value, before[key]):
            delta[key] = UNAVAILABLE
        elif key == "peak_rss_bytes":
            delta[key] = value
        else:
            delta[key] = round(value - before[key], 3)
    return delta


def read_only(database: Path) -> sqlite3.Connection:
    return sqlite3.connect(f"file:{database}?mode=ro", uri=True, timeout=5)


def published_locations(connection: sqlite3.Connection, root_ids: list[int]) -> int:
    marks = ",".join("?" * len(root_ids))
    return connection.execute(
        "SELECT count(*) FROM file_locations WHERE retired_at IS NULL "
        f"AND storage_root_id IN ({marks})", root_ids,
    ).fetchone()[0]


def session_states(connection: sqlite3.Connection, session_ids: list[int]) -> dict[int, str]:
    marks = ",".join("?" * len(session_ids))
    rows = connection.execute(
        f"SELECT id, status FROM scan_sessions WHERE id IN ({marks})", session_ids)
    return dict(rows.fetchall())


TERMINAL = {"succeeded", "failed", "cancelled", "stale"}


def wait_for_publication(database: Path, root_ids: list[int], session_ids: list[int],
                         started: float, limit: float,
                         alive: Callable[[], None]) -> tuple[Any, Any, dict[int, str]]:
    """Poll until every session is terminal; return first-durable and complete seconds.

    `alive` raises when the stack has exited or a stop was requested.
    """
    first = complete = UNAVAILABLE
    connection = read_only(database)
    try:
        while True:
            alive()
            elapsed = time.monotonic() - started
            if first == UNAVAILABLE and published_locations(connection, root_ids) > 0:
                first = round(elapsed, 3)
            states = session_states(connection, session_ids)
            if len(states) == len(session_ids) and set(states.values()) <= TERMINAL:
                complete = round(elapsed, 3)
                if first == UNAVAILABLE and published_locations(connection, root_ids) > 0:
                    first = complete
                return first, complete, states
            if elapsed > limit:
                raise HarnessError("scan sessions did not reach a terminal state in time")
            time.sleep(POLL_SECONDS)
    finally:
        connection.close()


def stored_evidence(database: Path, session_ids: list[int],
                    locators: dict[tuple[int, str], str]) -> dict[str, Any]:
    """Hash sources of stored evidence, and a digest of (manifest id, content hash) pairs."""
    marks = ",".join("?" * len(session_ids))
    connection = read_only(database)
    try:
        rows = connection.execute(
            "SELECT s.storage_root_id, o.provider_relative_locator, o.evidence_json "
            "FROM scan_observations o JOIN scan_sessions s ON s.id = o.scan_session_id "
            f"WHERE o.scan_session_id IN ({marks})", session_ids).fetchall()
    finally:
        connection.close()
    summary = {"observations": len(rows), "with_evidence": 0, "reused": 0,
               "recomputed_without_source": 0, "other_source": 0, "unmatched": 0}
    pairs = []
    for root_id, locator, evidence_json in rows:
        file_id = locators.get((root_id, locator))
        if file_id is None:
            summary["unmatched"] += 1
            continue
        if evidence_json is None:
            continue
        evidence = json.loads(evidence_json)
        summary["with_evidence"] += 1
        source = evidence.get("hash_source", None)
        key = {"reused": "reused", None: "recomputed_without_source"}.get(source, "other_source")
        summary[key] += 1
        pairs.append(f"{file_id} {evidence['content_hash']}\n")
    summary["hash_set_sha256"] = hashlib.sha256("".join(sorted(pairs)).encode()).hexdigest()
    return summary


def corpus_unchanged(manifest: dict[str, Any], files: list[dict[str, Any]]) -> bool:
    root = Path(manifest["root"])
    for entry in files:
        facts = (root / entry["relative"]).stat()
        if (facts.st_size, facts.st_mtime_ns) != (entry["size_bytes"], entry["mtime_ns"]):
            return False
    return True


def stack_args(workdir: Path, roots: list[Path], bin_dir: Path) -> argparse.Namespace:
    arguments = ["serve", "--workdir", str(workdir), "--bin-dir", str(bin_dir)]
    for root in roots:
        arguments += ["--input", str(root)]
    return bootstrap.parser().parse_args(arguments)


def request_scans(stack: Stack) -> list[int]:
    sessions = []
    for root in stack.roots:
        if root["role"] == "input":
            data = stack.voom("scan", "--root", root["root_id"], "--no-wait")["data"]
            sessions.append(data["scan_session_id"])
    return sessions


def kill_later(pid: int, delay: float) -> threading.Timer:
    """SIGKILL a worker mid-scan (O14); the agent restarts it, so usage is unavailable."""
    timer = threading.Timer(delay, os.kill, (pid, 9))
    timer.start()
    return timer


def scan_repetition(index: int, manifest: dict[str, Any], files: list[dict[str, Any]],
                    args: argparse.Namespace, fs_type: str) -> dict[str, Any]:
    roots = scan_roots(manifest, files)
    workdir = args.workdir / f"rep-{index}"
    with Stack(stack_args(workdir, roots, args.bin_dir), args.reuse == "on") as stack:
        corpus_root = Path(manifest["root"])
        by_root = {Path(root["provider_locator"]): root["root_id"] for root in stack.roots}
        locators = {}
        for entry in files:
            path = corpus_root / entry["relative"]
            root = next(candidate for candidate in roots if path.is_relative_to(candidate))
            locators[(by_root[root.resolve()], path.relative_to(root).as_posix())] = entry["id"]
        if args.evict:
            for entry in files:
                baseline.evict(corpus_root / entry["relative"], fs_type)
        residency = {entry["id"]: baseline.resident_bytes(corpus_root / entry["relative"])
                     for entry in files}
        agent = next(process for name, process in stack.children if name == "voom-node-agent")
        pids = worker_pids(agent.pid)
        before = {name: process_usage(pid) for name, pid in pids.items()}
        counters = baseline.host_counters()
        started = time.monotonic()
        sessions = request_scans(stack)
        timer = kill_later(pids[args.kill], args.kill_after) if args.kill else None
        first, complete, states = wait_for_publication(
            workdir / "voom.db", list(set(by_root.values())), sessions, started, args.limit,
            stack.require_alive)
        if timer:
            timer.cancel()
        counters = baseline.counter_delta(counters, baseline.host_counters())
        restarted = worker_pids(agent.pid) != pids
        workers = {name: dict.fromkeys(baseline.WORKER_KEYS, UNAVAILABLE) if restarted
                   else usage_delta(before[name], process_usage(pid))
                   for name, pid in pids.items()}
        evidence = stored_evidence(workdir / "voom.db", sessions, locators)
    return {
        "index": index, "evicted": args.evict, "resident_bytes_before": residency,
        "first_durable_observation_seconds": first, "complete_seconds": complete,
        "session_status": sorted(states.values()), "worker_restarted": restarted,
        "evidence": evidence, "workers": workers, "host_counters": counters,
        "corpus_unchanged": corpus_unchanged(manifest, files),
    }


def binary_identity(bin_dir: Path) -> dict[str, str]:
    return {name: baseline.sha256_file(bin_dir / name) for name in BINARIES}


def capture_scan(args: argparse.Namespace) -> dict[str, Any]:
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    files = baseline.select_files(manifest, args.ids)
    fs_type = baseline.corpus_filesystem(Path(manifest["root"]), files)
    if args.workdir.exists():
        raise HarnessError("the stack work directory must not exist yet; choose a new one")
    args.workdir.mkdir(parents=True, mode=0o700)
    repetitions = [scan_repetition(index, manifest, files, args, fs_type)
                   for index in range(1, args.repetitions + 1)]
    commit = baseline.git_output("rev-parse", "HEAD")
    return {
        "schema": SCAN_SCHEMA,
        "cell": {"probe_hash_reuse": args.reuse == "on", "evict": args.evict,
                 "repetitions": args.repetitions,
                 "kill": {"worker": args.kill, "after_seconds": args.kill_after}
                 if args.kill else None},
        "corpus": {"label": manifest["label"], "filesystem_type": fs_type,
                   "storage_class": baseline.storage_class(fs_type),
                   "manifest_files": len(manifest["files"]),
                   "selected_ids": [entry["id"] for entry in files],
                   "selected_bytes": sum(entry["size_bytes"] for entry in files),
                   "roots": len(scan_roots(manifest, files))},
        "build": {"git_commit": commit.strip() if commit else UNAVAILABLE,
                  "git_changed_paths": baseline.tracked_changes(),
                  "instrument_sha256": baseline.sha256_file(Path(__file__).resolve()),
                  "bootstrap_sha256": baseline.sha256_file(SCRIPTS / "acceptance-bootstrap.py"),
                  "binaries_sha256": binary_identity(args.bin_dir),
                  "ffprobe_version": baseline.ffprobe_version()},
        "host": baseline.host_identity(),
        "repetitions": repetitions,
        "not_measured": NOT_MEASURED,
    }


SCAN_KEYS = {
    "cell": {"probe_hash_reuse", "evict", "repetitions", "kill"},
    "corpus": {"label", "filesystem_type", "storage_class", "manifest_files", "selected_ids",
               "selected_bytes", "roots"},
    "build": {"git_commit", "git_changed_paths", "instrument_sha256", "bootstrap_sha256",
              "binaries_sha256", "ffprobe_version"},
    "host": {"os", "machine", "cpu_count", "memory_bytes", "python"},
}
SCAN_REPETITION_KEYS = {"index", "evicted", "resident_bytes_before",
                        "first_durable_observation_seconds", "complete_seconds",
                        "session_status", "worker_restarted", "evidence", "workers",
                        "host_counters", "corpus_unchanged"}
EVIDENCE_KEYS = {"observations", "with_evidence", "reused", "recomputed_without_source",
                 "other_source", "unmatched", "hash_set_sha256"}


def scan_repetition_problems(repetition: dict[str, Any]) -> list[str]:
    problems = baseline.missing("repetition", repetition, SCAN_REPETITION_KEYS)
    if problems:
        return problems
    problems += baseline.missing("evidence", repetition["evidence"], EVIDENCE_KEYS)
    for name in ("first_durable_observation_seconds", "complete_seconds"):
        if not baseline.is_metric(repetition[name]):
            problems.append(name)
    for name in WORKER_PROGRAMS:
        worker = repetition["workers"].get(name, {})
        problems += baseline.missing(f"worker {name}", worker, baseline.WORKER_KEYS)
        problems += [f"worker {name} metric" for value in worker.values()
                     if not baseline.is_metric(value)]
    return problems


def validate_scan(record: dict[str, Any]) -> list[str]:
    """Every contract violation in a committed-safe scan record."""
    if record.get("schema") != SCAN_SCHEMA:
        return ["schema"]
    problems = []
    for section, keys in SCAN_KEYS.items():
        problems += baseline.missing(section, record.get(section, {}), keys)
    if set(record.get("not_measured", {})) != set(NOT_MEASURED):
        problems.append("not_measured")
    if not record.get("repetitions"):
        problems.append("repetitions")
    for repetition in record.get("repetitions", []):
        problems += scan_repetition_problems(repetition)
    return problems + host_path_problems(record)


def host_path_problems(record: dict[str, Any]) -> list[str]:
    text = json.dumps(record)
    return ["host path"] if re.search(r'"/|\\\\|/home/|/Users/|/mnt/', text) else []


def same_size_edit(path: Path, restore_mtime: bool) -> None:
    """Invert one interior block in place; optionally restore the mtime (not the ctime)."""
    facts = path.stat()
    offset = facts.st_size // 2
    with path.open("r+b") as handle:
        handle.seek(offset)
        block = handle.read(EDIT_BYTES)
        handle.seek(offset)
        handle.write(bytes(byte ^ 0xFF for byte in block))
        handle.flush()
        os.fsync(handle.fileno())
    if restore_mtime:
        os.utime(path, ns=(facts.st_atime_ns, facts.st_mtime_ns))


def ordinary_write(path: Path) -> None:
    """A same-size write that moves the mtime, as an editor or tagger would."""
    time.sleep(0.02)
    same_size_edit(path, restore_mtime=False)


def outcome(stage: str, frame: dict[str, Any]) -> dict[str, Any]:
    """Class, code and the fact-free stage marker only: messages can carry host paths."""
    if frame.get("kind") == "result":
        payload = frame.get("payload") or {}
        return {"stage": stage, "result": "result", "hash_source": payload.get("hash_source")}
    detail = frame.get("payload") if isinstance(frame.get("payload"), dict) else {}
    return {"stage": stage, "result": "failure", "class": frame.get("class"),
            "code": frame.get("code"), "marker": detail.get("stage")}


def probe(workers: dict[str, Any], path: Path, facts: dict[str, Any]) -> dict[str, Any]:
    payload = {
        "path": str(path),
        "expected": {"size_bytes": facts["size_bytes"], "content_hash": facts["content_hash"],
                     "modified_at": facts["modified_at"], "local_file_key": None},
    }
    if facts.get("changed_at_ns") is not None and facts.get("file_key") is not None:
        payload["guard"] = {"changed_at_ns": facts["changed_at_ns"],
                            "file_key": facts["file_key"]}
    return outcome("probe", workers["probe"].call("probe_file", payload))


def hash_file(workers: dict[str, Any], path: Path) -> dict[str, Any]:
    return workers["hash"].call("hash_file", {
        "provider_locator": str(path.parent), "provider_relative_locator": path.name})


def hash_then_probe(workers: dict[str, Any], path: Path, between: Any) -> dict[str, Any]:
    hashed = hash_file(workers, path)
    if hashed.get("kind") != "result":
        return outcome("hash", hashed)
    if between:
        between(path)
    return probe(workers, path, hashed["payload"])


def edit_during_hash(workers: dict[str, Any], path: Path, delay: float) -> dict[str, Any]:
    """Case (b): edit while the hash worker reads, mtime restored before its post-read stat."""
    edited_at: list[float] = []

    def edit() -> None:
        same_size_edit(path, restore_mtime=True)
        edited_at.append(time.monotonic())

    timer = threading.Timer(delay, edit)
    timer.start()
    hashed = hash_file(workers, path)
    finished = time.monotonic()
    timer.join()
    if not edited_at or edited_at[0] >= finished:
        raise HarnessError("case (b) edit landed after the hash read ended; lower --edit-delay")
    return outcome("hash", hashed)


CASES = {
    "unchanged": lambda workers, path, args: hash_then_probe(workers, path, None),
    "a_restored_mtime_edit_before_probe": lambda workers, path, args: hash_then_probe(
        workers, path, lambda target: same_size_edit(target, restore_mtime=True)),
    "b_restored_mtime_edit_during_hash": lambda workers, path, args: edit_during_hash(
        workers, path, args.edit_delay),
    "c_mtime_moving_write_before_probe": lambda workers, path, args: hash_then_probe(
        workers, path, ordinary_write),
}


def copy_source(source: Path, destination: Path) -> None:
    """A private copy: a reflink where the filesystem allows one, else a byte copy."""
    subprocess.run(["cp", "--reflink=auto", "--", str(source), str(destination)], check=True)


def run_case(name: str, args: argparse.Namespace, fs_type: str) -> dict[str, Any]:
    copy = args.scratch / f"case-{name}{args.source.suffix}"
    copy_source(args.source, copy)
    try:
        if args.evict:
            baseline.evict(copy, fs_type)
        workers = {name: baseline.Worker(args.bin_dir / program)
                   for name, program in WORKER_PROGRAMS.items()}
        try:
            result = CASES[name](workers, copy, args)
        finally:
            for worker in workers.values():
                worker.stop()
    finally:
        copy.unlink()
    # A case is detected only by the guard's own failure, never by an unrelated error.
    result["case"] = name
    result["detected"] = (result.get("code") == MISMATCH) if name != "unchanged" else None
    return result


def capture_construct(args: argparse.Namespace) -> dict[str, Any]:
    if not args.scratch.is_dir() or any(args.scratch.iterdir()):
        raise HarnessError("--scratch must be an existing empty directory")
    fs_type = baseline.filesystem_type(args.scratch)
    cases = [run_case(name, args, fs_type) for name in CASES]
    commit = baseline.git_output("rev-parse", "HEAD")
    return {
        "schema": CONSTRUCT_SCHEMA,
        "scratch": {"filesystem_type": fs_type, "storage_class": baseline.storage_class(fs_type),
                    "source_label": args.label, "source_bytes": args.source.stat().st_size},
        "build": {"git_commit": commit.strip() if commit else UNAVAILABLE,
                  "instrument_sha256": baseline.sha256_file(Path(__file__).resolve()),
                  "binaries_sha256": {program: baseline.sha256_file(args.bin_dir / program)
                                      for program in WORKER_PROGRAMS.values()},
                  "ffprobe_version": baseline.ffprobe_version()},
        "cases": cases,
    }


def validate_construct(record: dict[str, Any]) -> list[str]:
    if record.get("schema") != CONSTRUCT_SCHEMA:
        return ["schema"]
    problems = []
    names = [case.get("case") for case in record.get("cases", [])]
    if names != list(CASES):
        problems.append("cases")
    for case in record.get("cases", []):
        problems += baseline.missing(f"case {case.get('case')}", case,
                                     {"case", "stage", "result", "detected"})
    return problems + host_path_problems(record)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    scan = commands.add_parser("scan", help="time real scan sessions to first durable observation")
    scan.add_argument("manifest", type=Path)
    scan.add_argument("--reuse", choices=["on", "off"], required=True)
    scan.add_argument("--ids", help="comma-separated manifest ids; default is every file")
    scan.add_argument("--repetitions", type=int, default=3)
    scan.add_argument("--evict", action="store_true",
                      help="drop page cache for selected files first (local storage only)")
    scan.add_argument("--workdir", type=Path, required=True,
                      help="new private directory for the per-repetition stacks")
    scan.add_argument("--bin-dir", type=Path, default=baseline.REPO_ROOT / "target" / "release")
    scan.add_argument("--limit", type=float, default=3600.0,
                      help="seconds to wait for every session to end")
    scan.add_argument("--kill", choices=list(WORKER_PROGRAMS),
                      help="SIGKILL this worker --kill-after seconds into each repetition")
    scan.add_argument("--kill-after", type=float, default=5.0)
    scan.add_argument("--output", type=Path, required=True)
    construct = commands.add_parser("construct", help="build the ADR 0120 guard cases")
    construct.add_argument("source", type=Path, help="media file to copy for each case")
    construct.add_argument("--label", required=True, help="public label for the source file")
    construct.add_argument("--scratch", type=Path, required=True,
                           help="empty directory on the filesystem under test")
    construct.add_argument("--evict", action="store_true",
                           help="drop each copy's page cache before its case")
    construct.add_argument("--edit-delay", type=float, default=0.5,
                           help="seconds into the hash read before case (b) edits")
    construct.add_argument("--bin-dir", type=Path,
                           default=baseline.REPO_ROOT / "target" / "release")
    construct.add_argument("--output", type=Path, required=True)
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        if args.command == "scan":
            record, validate = capture_scan(args), validate_scan
        else:
            record, validate = capture_construct(args), validate_construct
        problems = validate(record)
        if problems:
            raise HarnessError("record failed validation: " + ", ".join(problems))
        baseline.write_new(args.output, record, 0o644)
    except (HarnessError, RuntimeError, OSError, ValueError, KeyError, InterruptedError,
            subprocess.SubprocessError, sqlite3.Error) as error:
        print(f"media-evidence-rollout: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
