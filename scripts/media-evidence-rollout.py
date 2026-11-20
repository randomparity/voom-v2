#!/usr/bin/env python3
"""Rollout instrument for #794: publishing scans and constructed guard cases.

`scan` runs a real local control plane, owner node agent and bundled workers through
`acceptance-bootstrap.py` (#339's stack), requests one scan session per root, and times each
repetition from the request to its first durable observation: the first live rooted file
location the stack publishes. Completion publication (ADR 0077) publishes inside the
completion transaction; the #841 per-file prototype publishes at batch acceptance. It records
worker CPU, memory and I/O, the stored evidence's hash sources, and a digest of the published
content hashes, so repeat scans compare without committing hashes. `serve` holds such a stack for the
CLI-driven transform gates. `construct` drives the release hash and probe workers on a
scratch copy to build the ADR 0120 guard cases on the filesystem that holds the copy.
Records name files by manifest id only; host paths stay in private manifests and stack work
directories outside the checkout.
"""

from __future__ import annotations

import argparse
import getpass
import hashlib
import importlib.util
import json
import os
import re
import select
import shutil
import signal
import socket
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
candidates = load("media_evidence_candidates", "media-evidence-candidates.py")

UNAVAILABLE = baseline.UNAVAILABLE
HarnessError = baseline.HarnessError
SCAN_SCHEMA_V1 = "media-evidence-rollout-scan/1"
SCAN_SCHEMA = "media-evidence-rollout-scan/2"
PUBLICATIONS = ("completion", "per-file")
# The #841 prototype's switches; a build without them refuses both (clap, deny_unknown_fields).
PER_FILE_AGENT_KEY = "scan_per_file_publication"
PER_FILE_API_FLAG = "--scan-per-file-publication"
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
    "ffprobe_child": "ffprobe's memory is not attributed to the probe worker; its CPU and "
                     "I/O are, once reaped",
}


class Stack(bootstrap.Bootstrap):
    """The #339 acceptance stack with the owner-node reuse setting written explicitly.

    `per-file` publication also turns on the #841 prototype in the API and the agent.
    """

    def __init__(self, args: argparse.Namespace, reuse: bool, publication: str = "completion"):
        if publication not in PUBLICATIONS:
            raise HarnessError(f"unknown publication {publication!r}")
        self.reuse = reuse
        self.publication = publication
        super().__init__(args)

    def configuration(self, credential: Path, base_url: str) -> str:
        setting = f"probe_hash_reuse = {'true' if self.reuse else 'false'}\n"
        if self.publication == "per-file":
            setting += f"{PER_FILE_AGENT_KEY} = true\n"
        return setting + super().configuration(credential, base_url)

    def launch(self, name: str, arguments: list[str]) -> Any:
        if name == "voom-api" and self.publication == "per-file":
            arguments = [*arguments, PER_FILE_API_FLAG]
        return super().launch(name, arguments)


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
    """CPU and I/O include reaped children such as ffprobe; memory is this process only."""
    return baseline.proc_usage(pid, "media-evidence-rollout")


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
    # `as_uri` percent-encodes `#`, `?` and `%`, which would otherwise end the path early.
    return sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True, timeout=5)


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


def observation_batches(database: Path, session_ids: list[int]) -> int:
    """Accepted batches of these sessions: one per observation proves the agent flushed each."""
    marks = ",".join("?" * len(session_ids))
    connection = read_only(database)
    try:
        return connection.execute(
            f"SELECT count(*) FROM scan_observation_batches WHERE scan_session_id IN ({marks})",
            session_ids).fetchone()[0]
    finally:
        connection.close()


def wait_for_publication(database: Path, root_ids: list[int], session_ids: list[int],
                         started: float, limit: float, alive: Callable[[], None],
                         ) -> tuple[Any, Any, dict[int, str], int]:
    """Poll until every session is terminal.

    Returns first-durable and complete seconds, the terminal states, and the most live
    locations any poll saw while some session was still running: above zero only when
    publication preceded completion. `alive` raises when the stack has exited or a stop was
    requested.
    """
    first = complete = UNAVAILABLE
    before_complete = 0
    connection = read_only(database)
    try:
        while True:
            alive()
            elapsed = time.monotonic() - started
            # Read publication first: a location seen while a session still runs was
            # published before that session's completion committed.
            published = published_locations(connection, root_ids)
            states = session_states(connection, session_ids)
            if len(states) == len(session_ids) and set(states.values()) <= TERMINAL:
                complete = round(elapsed, 3)
                if first == UNAVAILABLE and published_locations(connection, root_ids) > 0:
                    first = complete
                return first, complete, states, before_complete
            before_complete = max(before_complete, published)
            if first == UNAVAILABLE and published > 0:
                first = round(elapsed, 3)
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
        # G8: a recomputed row carries no hash source field at all, not even a null one.
        if "hash_source" not in evidence:
            summary["recomputed_without_source"] += 1
        elif evidence["hash_source"] == "reused":
            summary["reused"] += 1
        else:
            summary["other_source"] += 1
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


def stack_args(workdir: Path, roots: list[Path], bin_dir: Path,
               output: Path | None = None) -> argparse.Namespace:
    arguments = ["serve", "--workdir", str(workdir), "--bin-dir", str(bin_dir)]
    for root in roots:
        arguments += ["--input", str(root)]
    if output is not None:
        arguments += ["--output", str(output)]
    return bootstrap.parser().parse_args(arguments)


def serve(args: argparse.Namespace) -> None:
    """Hold a stack with the reuse setting until stdin closes, for CLI-driven gates."""
    with Stack(stack_args(args.workdir, args.input, args.bin_dir, args.output),
               args.reuse == "on") as stack:
        print(json.dumps(stack.receipt), flush=True)
        while not stack.stopping:
            readable, _, _ = select.select([sys.stdin], [], [], 0.2)
            if readable and not os.read(sys.stdin.fileno(), 1):
                break
            stack.require_alive()


def request_scans(stack: Stack) -> list[int]:
    sessions = []
    for root in stack.roots:
        if root["role"] == "input":
            data = stack.voom("scan", "--root", root["root_id"], "--no-wait")["data"]
            sessions.append(data["scan_session_id"])
    return sessions


def start_time(pid: int) -> str | None:
    try:
        return Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
    except (OSError, IndexError):
        return None


def kill_at(pid: int, at: float) -> threading.Timer:
    """SIGKILL a worker at monotonic time `at` (O14), only if the PID is still that worker."""
    identity = start_time(pid)

    def kill() -> None:
        if identity is not None and start_time(pid) == identity:
            os.kill(pid, signal.SIGKILL)

    timer = threading.Timer(max(0.0, at - time.monotonic()), kill)
    timer.start()
    return timer


def scan_repetition(index: int, manifest: dict[str, Any], files: list[dict[str, Any]],
                    args: argparse.Namespace, fs_type: str) -> dict[str, Any]:
    roots = scan_roots(manifest, files)
    workdir = args.workdir / f"rep-{index}"
    with Stack(stack_args(workdir, roots, args.bin_dir), args.reuse == "on",
               args.publication) as stack:
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
        timer = kill_at(pids[args.kill], started + args.kill_after) if args.kill else None
        try:
            sessions = request_scans(stack)
            first, complete, states, before_complete = wait_for_publication(
                workdir / "voom.db", list(set(by_root.values())), sessions, started,
                args.limit, stack.require_alive)
        finally:
            if timer:
                timer.cancel()
        counters = baseline.counter_delta(counters, baseline.host_counters())
        restarted = worker_pids(agent.pid) != pids
        workers = {name: dict.fromkeys(baseline.WORKER_KEYS, UNAVAILABLE) if restarted
                   else usage_delta(before[name], process_usage(pid))
                   for name, pid in pids.items()}
        evidence = stored_evidence(workdir / "voom.db", sessions, locators)
        batches = observation_batches(workdir / "voom.db", sessions)
    return {
        "index": index, "evicted": args.evict, "resident_bytes_before": residency,
        "first_durable_observation_seconds": first, "complete_seconds": complete,
        "session_status": sorted(states.values()), "worker_restarted": restarted,
        "batches": batches, "published_before_complete": before_complete,
        "evidence": evidence, "workers": workers, "host_counters": counters,
        "corpus_unchanged": corpus_unchanged(manifest, files),
    }


def binary_identity(bin_dir: Path) -> dict[str, str]:
    return {name: baseline.sha256_file(bin_dir / name) for name in BINARIES}


def binaries_build(bin_dir: Path) -> dict[str, Any]:
    """The binaries and the checkout that holds them; a dirty checkout is not reproducible."""
    commit = baseline.git_output("rev-parse", "HEAD", repo_root=bin_dir)
    changed = baseline.tracked_changes(repo_root=bin_dir)
    if commit is None or changed:
        raise HarnessError("--bin-dir must sit in a clean git checkout of the built source")
    return {"binaries_sha256": binary_identity(bin_dir),
            "binaries_git_commit": commit.strip(), "binaries_git_changed_paths": changed}


def capture_scan(args: argparse.Namespace) -> dict[str, Any]:
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    public_label(manifest["label"])
    files = baseline.select_files(manifest, args.ids)
    root_count = len(scan_roots(manifest, files))
    if args.publication == "per-file" and root_count != 1:
        raise HarnessError(f"per-file publication proves one root only; this selection has "
                           f"{root_count} roots (use a full pass or a single file)")
    fs_type = baseline.corpus_filesystem(Path(manifest["root"]), files)
    if args.workdir.exists():
        raise HarnessError("the stack work directory must not exist yet; choose a new one")
    args.workdir.mkdir(parents=True, mode=0o700)
    built = binaries_build(args.bin_dir)
    repetitions = [scan_repetition(index, manifest, files, args, fs_type)
                   for index in range(1, args.repetitions + 1)]
    if binaries_build(args.bin_dir) != built:
        raise HarnessError("the binaries or their checkout changed during the capture")
    commit = baseline.git_output("rev-parse", "HEAD")
    return {
        "schema": SCAN_SCHEMA,
        "cell": {"probe_hash_reuse": args.reuse == "on", "publication": args.publication,
                 "evict": args.evict,
                 "repetitions": args.repetitions,
                 "kill": {"worker": args.kill, "after_seconds": args.kill_after}
                 if args.kill else None},
        "corpus": {"label": manifest["label"], "filesystem_type": fs_type,
                   "storage_class": baseline.storage_class(fs_type),
                   "manifest_files": len(manifest["files"]),
                   "selected_ids": [entry["id"] for entry in files],
                   "selected_bytes": sum(entry["size_bytes"] for entry in files),
                   "roots": root_count},
        "build": {"git_commit": commit.strip() if commit else UNAVAILABLE,
                  "git_changed_paths": baseline.tracked_changes(),
                  "instrument_sha256": baseline.sha256_file(Path(__file__).resolve()),
                  "bootstrap_sha256": baseline.sha256_file(SCRIPTS / "acceptance-bootstrap.py"),
                  **built,
                  "ffprobe_version": baseline.ffprobe_version()},
        "host": baseline.host_identity(),
        "repetitions": repetitions,
        "not_measured": NOT_MEASURED,
    }


SCAN_KEYS_V1 = {
    "cell": {"probe_hash_reuse", "evict", "repetitions", "kill"},
    "corpus": {"label", "filesystem_type", "storage_class", "manifest_files", "selected_ids",
               "selected_bytes", "roots"},
    "build": {"git_commit", "git_changed_paths", "instrument_sha256", "bootstrap_sha256",
              "binaries_sha256", "ffprobe_version"},
    "host": {"os", "machine", "cpu_count", "memory_bytes", "python"},
}
SCAN_REPETITION_KEYS_V1 = {"index", "evicted", "resident_bytes_before",
                           "first_durable_observation_seconds", "complete_seconds",
                           "session_status", "worker_restarted", "evidence", "workers",
                           "host_counters", "corpus_unchanged"}
# Schema /2 (#841) adds the publication setting, its proof, and the binaries' own checkout.
SCAN_KEYS = dict(SCAN_KEYS_V1, cell=SCAN_KEYS_V1["cell"] | {"publication"},
                 build=SCAN_KEYS_V1["build"] | {"binaries_git_commit",
                                                "binaries_git_changed_paths"})
SCAN_REPETITION_KEYS = SCAN_REPETITION_KEYS_V1 | {"batches", "published_before_complete"}
EVIDENCE_KEYS = {"observations", "with_evidence", "reused", "recomputed_without_source",
                 "other_source", "unmatched", "hash_set_sha256"}


def scan_repetition_problems(repetition: dict[str, Any],
                             keys: set[str] = SCAN_REPETITION_KEYS) -> list[str]:
    problems = baseline.missing("repetition", repetition, keys)
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
    """Every contract violation in a committed-safe scan record, `/1` or `/2`."""
    if record.get("schema") == SCAN_SCHEMA_V1:
        return scan_problems(record, SCAN_KEYS_V1, SCAN_REPETITION_KEYS_V1)
    if record.get("schema") != SCAN_SCHEMA:
        return ["schema"]
    problems = scan_problems(record, SCAN_KEYS, SCAN_REPETITION_KEYS)
    return problems or publication_problems(record)


def read_compared(path: Path) -> tuple[Any, list[str]]:
    """A record named on the `compare` command line, with every reason it may not be judged."""
    try:
        record = json.loads(path.read_text(encoding="utf-8"))
    except ValueError:
        return None, ["not valid JSON"]
    if not isinstance(record, dict):
        return record, ["not a JSON object"]
    try:
        return record, validate_scan(record)
    except (AttributeError, TypeError) as error:
        # A hand-edited section of the wrong JSON type; the validator reads sections as objects.
        return record, [f"malformed sections ({error})"]


def compared_records(args: argparse.Namespace) -> list[dict[str, Any]]:
    """Both `compare` inputs, refused together, by role and path, unless each passes validation."""
    records, refusals = [], []
    for role, path in (("baseline", args.baseline), ("candidate", args.candidate)):
        record, problems = read_compared(path)
        records.append(record)
        if problems:
            refusals.append(f"{role} {path} failed validation: " + ", ".join(problems))
    if refusals:
        raise HarnessError("; ".join(refusals))
    return records


def publication_problems(record: dict[str, Any]) -> list[str]:
    """A `/2` cell must have run as labelled: every session done, per-file proven both ways."""
    problems = []
    if record["cell"]["publication"] not in PUBLICATIONS:
        problems.append("publication")
    # The proof's batches and publication are cell-wide: with a second root, that root's
    # in-session publication would cover one that published only at completion.
    if record["cell"]["publication"] == "per-file" and record["corpus"]["roots"] != 1:
        problems.append("per-file roots")
    if record["build"]["binaries_git_changed_paths"]:
        problems.append("binaries checkout dirty")
    for repetition in record["repetitions"]:
        # A failed session can still publish under per-file, so it is never a clean cell.
        if not record["cell"]["kill"] and set(repetition["session_status"]) != {"succeeded"}:
            problems.append(f"repetition {repetition['index']} sessions")
        if record["cell"]["publication"] != "per-file":
            continue
        if repetition["batches"] != repetition["evidence"]["observations"]:
            problems.append(f"repetition {repetition['index']} per-file batches")
        if repetition["published_before_complete"] <= 0:
            problems.append(f"repetition {repetition['index']} per-file publication")
    return problems


def scan_problems(record: dict[str, Any], keys: dict[str, set[str]],
                  repetition_keys: set[str]) -> list[str]:
    problems = []
    # A kill that never landed would pass a clean scan off as an interruption cell.
    if record.get("cell", {}).get("kill") and not all(
            repetition.get("worker_restarted") for repetition in record.get("repetitions", [])):
        problems.append("kill not delivered")
    for section, section_keys in keys.items():
        problems += baseline.missing(section, record.get(section, {}), section_keys)
    if set(record.get("not_measured", {})) != set(NOT_MEASURED):
        problems.append("not_measured")
    if not record.get("repetitions"):
        problems.append("repetitions")
    for repetition in record.get("repetitions", []):
        problems += scan_repetition_problems(repetition, repetition_keys)
    return problems + host_path_problems(record)


def host_path_problems(record: dict[str, Any]) -> list[str]:
    text = json.dumps(record)
    return ["host path"] if re.search(r'"/|\\\\|/(home|Users|mnt)/', text) else []


def scan_metrics(record: dict[str, Any], repetition: dict[str, Any]) -> dict[str, Any]:
    workers = repetition["workers"]
    physical = [worker["read_bytes"] for worker in workers.values()]
    if record["corpus"]["storage_class"] == "network":
        physical = [repetition["host_counters"]["cifs_bytes_read"]]
    return {
        "first_durable_observation": repetition["first_durable_observation_seconds"],
        "complete": repetition["complete_seconds"],
        "physical_bytes": UNAVAILABLE if UNAVAILABLE in physical else sum(physical),
        "probe_rchar": workers["probe"]["rchar"],
        "hash_cpu": workers["hash"]["cpu_seconds"],
        "probe_cpu": workers["probe"]["cpu_seconds"],
    }


def compare_scans(baseline_record: dict[str, Any], candidate: dict[str, Any]) -> dict[str, Any]:
    """Protocol verdicts per metric; an as-found cell compares repetitions 2 and later."""
    for section, key in (("corpus", "label"), ("corpus", "selected_ids"), ("cell", "evict"),
                         ("build", "binaries_sha256")):
        if baseline_record[section][key] != candidate[section][key]:
            raise HarnessError(f"cells differ in {section} {key}; they do not compare")
    if baseline_record["cell"]["kill"] or candidate["cell"]["kill"]:
        raise HarnessError("kill cells do not compare")
    # Exactly one setting moves from its baseline value to its treatment value.
    settings = [(record["cell"]["probe_hash_reuse"],
                 record["cell"].get("publication", "completion") == "per-file")
                for record in (baseline_record, candidate)]
    turned_on = [not before and after for before, after in zip(*settings)]
    if settings[0] == settings[1] or turned_on.count(True) != 1 or \
            any(before and not after for before, after in zip(*settings)):
        raise HarnessError("compare a baseline with a candidate that turns exactly one setting "
                           "on (reuse, or per-file publication), no kills")
    skip = 0 if baseline_record["cell"]["evict"] else 1
    series = []
    for record in (baseline_record, candidate):
        rows = [scan_metrics(record, repetition) for repetition in record["repetitions"][skip:]]
        series.append({name: [row[name] for row in rows] for name in rows[0]} if rows else {})
    if not series[0] or not series[1]:
        raise HarnessError("a cell has no compared repetition; as-found cells need two or more")
    return {name: candidates.classify(series[0][name], series[1][name]) for name in series[0]}


def same_size_edit(path: Path, restore_mtime: bool) -> None:
    """Invert one interior block in place; optionally restore the mtime (not the ctime)."""
    descriptor = os.open(path, os.O_RDWR | os.O_NOFOLLOW)
    try:
        facts = os.fstat(descriptor)
        offset = facts.st_size // 2
        block = os.pread(descriptor, EDIT_BYTES, offset)
        os.pwrite(descriptor, bytes(byte ^ 0xFF for byte in block), offset)
        os.fsync(descriptor)
        if restore_mtime:
            os.utime(descriptor, ns=(facts.st_atime_ns, facts.st_mtime_ns))
    finally:
        os.close(descriptor)


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
    if facts.get("changed_at_ns") is None or facts.get("file_key") is None:
        raise HarnessError("the hash result lacks ctime or file key; no guard can be sent")
    payload["guard"] = {"changed_at_ns": facts["changed_at_ns"], "file_key": facts["file_key"]}
    result = outcome("probe", workers["probe"].call("probe_file", payload))
    result["guard_sent"] = True
    return result


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


def start_edit(delay: float, edit: Callable[[], None]) -> threading.Timer:
    """Run case (b)'s edit `delay` seconds into the hash read; the selftest replaces this."""
    timer = threading.Timer(delay, edit)
    timer.start()
    return timer


def edit_during_hash(workers: dict[str, Any], path: Path, delay: float) -> dict[str, Any]:
    """Case (b): edit while the hash worker reads, mtime restored before its post-read stat."""
    edited_at: list[float] = []

    def edit() -> None:
        same_size_edit(path, restore_mtime=True)
        edited_at.append(time.monotonic())

    pending = start_edit(delay, edit)
    hashed = hash_file(workers, path)
    finished = time.monotonic()
    pending.join()
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


def copy_bytes(source: Path, destination: Path) -> None:
    """A reflink where the filesystem allows one (GNU cp), else a byte copy."""
    if sys.platform.startswith("linux"):
        subprocess.run(["cp", "--reflink=auto", "--", str(source), str(destination)],
                       check=True)
    else:
        shutil.copyfile(source, destination)


def copy_source(source: Path, destination: Path) -> None:
    """A private copy; edits go only to it, so it must not be the source's inode."""
    copy_bytes(source, destination)
    copy, original = destination.lstat(), source.stat()
    if not destination.is_file() or destination.is_symlink() or \
            (copy.st_dev, copy.st_ino) == (original.st_dev, original.st_ino):
        raise HarnessError("the scratch copy is not a separate regular file; refusing to edit")


def run_case(name: str, args: argparse.Namespace, fs_type: str) -> dict[str, Any]:
    copy = args.scratch / f"case-{name}{args.source.suffix}"
    copy_source(args.source, copy)
    try:
        if args.evict:
            baseline.evict(copy, fs_type)
        workers = {role: baseline.Worker(args.bin_dir / program)
                   for role, program in WORKER_PROGRAMS.items()}
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


def public_label(label: str) -> str:
    """Labels reach committed records, so they may not name this host or user."""
    host = socket.gethostname().split(".")[0].lower()
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", label) or \
            any(name and name in label for name in (host, getpass.getuser().lower())):
        raise HarnessError("--label must be lowercase letters, digits and hyphens, "
                           "and must not name this host or user")
    return label


def capture_construct(args: argparse.Namespace) -> dict[str, Any]:
    public_label(args.label)
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
        # A probe without the guard would test the recompute path, not reuse.
        if case.get("stage") == "probe" and case.get("guard_sent") is not True:
            problems.append(f"case {case.get('case')} guard")
    return problems + host_path_problems(record)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    scan = commands.add_parser("scan", help="time real scan sessions to first durable observation")
    scan.add_argument("manifest", type=Path)
    scan.add_argument("--reuse", choices=["on", "off"], required=True)
    scan.add_argument("--publication", choices=PUBLICATIONS, default="completion",
                      help="per-file needs a build with the #841 prototype")
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
                      help="SIGKILL this worker --kill-after seconds after each scan request")
    scan.add_argument("--kill-after", type=float, default=5.0)
    scan.add_argument("--output", type=Path, required=True)
    judge = commands.add_parser("compare", help="judge a scan cell against its stage-0 cell")
    judge.add_argument("baseline", type=Path)
    judge.add_argument("candidate", type=Path)
    hold = commands.add_parser("serve", help="hold a stack for CLI-driven gates (G6, G9)")
    hold.add_argument("--reuse", choices=["on", "off"], required=True)
    hold.add_argument("--input", type=Path, action="append", required=True)
    hold.add_argument("--output", type=Path, help="output root (default: inside --workdir)")
    hold.add_argument("--workdir", type=Path, required=True)
    hold.add_argument("--bin-dir", type=Path, default=baseline.REPO_ROOT / "target" / "release")
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
        if args.command == "compare":
            print(json.dumps(compare_scans(*compared_records(args)), indent=2))
            return 0
        if args.command == "serve":
            serve(args)
            return 0
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
