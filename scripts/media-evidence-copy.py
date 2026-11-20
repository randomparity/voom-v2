#!/usr/bin/env python3
"""Node-agent commit copy loop measurement for issue #898.

`run` copies every selected manifest file once, one at a time, through the `copy-commit`
example, which calls the node agent's production `stream_copy_with_hash` (hash each chunk,
`write_all` it to a new file, then flush and fsync) into a scratch directory on the corpus's
filesystem and removes each copy. It writes a committed-safe record: corpora by label, files by
ordinal id, and one SHA-256 over the sorted per-file content hashes. `compare` judges a 1 MiB
cell against its 16 KiB cell with the thresholds frozen in docs/media-evidence-protocol.md and
the verdict rule in docs/workflow/specs/2026-11-19-node-agent-copy-loop-898-design.md. Build
the example with `media-evidence-candidates.py prepare copy16k|copy1m`.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


def _load(name: str, filename: str) -> Any:
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


observe = _load("media_evidence_observe", "media-evidence-observe.py")
baseline = observe.baseline
candidates = observe.candidates

SCHEMA = "media-evidence-copy-run/1"
PATH = "node-agent-copy"
UNAVAILABLE = baseline.UNAVAILABLE
HarnessError = baseline.HarnessError
REPO_ROOT = baseline.REPO_ROOT
COPY_SOURCE = candidates.COPY_SOURCE
BUFFER_PATTERN = r"const READ_BUFFER_BYTES: usize = ([0-9_ *]+);"
# The coordinator drives commit intents one at a time; one copy at a time also bounds the
# scratch space to the largest selected file.
CONCURRENCY = 1
OUTCOMES = {"copied", "copy_failed"}
NOT_MEASURED = {
    "install": "the coordinator's hard-link install, parent fsync and the hashes around the "
               "copy do not run",
    "kernel_writeback_cpu": "writeback and filesystem compression run in kernel threads, "
                            "outside the process's CPU",
    "removal": "each copy is removed inside its copy_seconds and the process's counters",
    "network_corpus": "excluded from #898; owned by the maintainer",
}


def buffer_bytes(repo_root: Path = REPO_ROOT) -> int:
    return baseline.read_constant(repo_root, "copy_buffer_bytes", COPY_SOURCE, BUFFER_PATTERN)


def _io_counters(text: str) -> dict[str, int]:
    counters = {}
    for line in text.splitlines():
        key, value = line.split(":")
        if key in IO_COUNTERS:
            counters[key] = int(value)
    return counters


IO_COUNTERS = ("rchar", "read_bytes", "wchar", "write_bytes")
PROC_SOURCES = (("stat", observe._stat_counters, ("cpu_seconds",)),
                ("status", observe._status_counters, ("peak_rss_bytes",)),
                ("io", _io_counters, IO_COUNTERS))


def process_usage(pid: int, proc_root: Path = Path("/proc")) -> dict[str, int | float | str]:
    """Observe's counters plus the process's write counters; a missing one is `unavailable`."""
    usage: dict[str, int | float | str] = {}
    for source, parse, counters in PROC_SOURCES:
        try:
            found = parse((proc_root / str(pid) / source).read_text())
            cause = f"{source}: field absent"
        except (OSError, IndexError, ValueError) as error:
            found, cause = {}, f"{source}: {type(error).__name__}"
        for counter in counters:
            usage[counter] = found.get(counter, UNAVAILABLE)
            if counter not in found:
                print(f"media-evidence-copy: process {counter} unavailable: {cause}",
                      file=sys.stderr)
    return usage


def check_scratch(scratch: Path, root: Path) -> None:
    """Refuse a scratch directory that could mix copies with media or other filesystems."""
    scratch, root = scratch.resolve(), root.resolve()
    if not scratch.is_dir():
        raise HarnessError("--scratch-dir must be an existing directory")
    if scratch == root or scratch.is_relative_to(root):
        raise HarnessError("--scratch-dir is inside the corpus root; use a sibling directory")
    if scratch.stat().st_dev != root.stat().st_dev:
        raise HarnessError("--scratch-dir is not on the corpus root's filesystem")
    check_empty(scratch)


def check_empty(scratch: Path) -> None:
    if any(scratch.iterdir()):
        raise HarnessError("--scratch-dir is not empty; remove its contents first")


def copy_all(example: Path, scratch: Path,
             paths: list[Path]) -> tuple[list[str], float, dict[str, Any]]:
    """Run one example process over `paths`; return its lines, wall seconds and usage."""
    if any("\n" in str(path) for path in paths):
        raise HarnessError("a selected path contains a newline; the example reads lines")
    process = subprocess.Popen(
        [str(example), str(CONCURRENCY), str(scratch)], stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
    )
    try:
        started = time.monotonic()
        process.stdin.write("".join(f"{path}\n" for path in paths) + "\n")
        process.stdin.flush()
        lines = []
        for line in process.stdout:
            if line == "done\n":
                break
            lines.append(line.rstrip("\n"))
        else:
            raise HarnessError(f"{example.name} exited before reporting done")
        wall = time.monotonic() - started
        usage = process_usage(process.pid)
    finally:
        process.stdin.close()
        try:
            process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        process.stdout.close()
    return lines, wall, usage


def parse_rows(lines: list[str], files: list[dict[str, Any]]) -> tuple[list, list[str]]:
    """Observe's rows renamed for a copy: `copy_seconds`, outcome `copied` or `copy_failed`."""
    rows, hashes = observe.parse_rows(lines, files)
    for row in rows:
        row["copy_seconds"] = row.pop("observe_seconds")
        row["outcome"] = "copied" if row["outcome"] == "observed" else "copy_failed"
    return rows, hashes


def run_repetition(index: int, root: Path, files: list[dict[str, Any]],
                   args: argparse.Namespace, fs_type: str, example: Path) -> dict[str, Any]:
    paths = [root / entry["relative"] for entry in files]
    if args.evict:
        for path in paths:
            baseline.evict(path, fs_type)
    residency = [baseline.resident_bytes(path) for path in paths]
    counters = baseline.host_counters()
    lines, wall, usage = copy_all(example, args.scratch_dir, paths)
    counters = baseline.counter_delta(counters, baseline.host_counters())
    check_empty(args.scratch_dir)
    rows, hashes = parse_rows(lines, files)
    for row, resident in zip(rows, residency):
        row["resident_bytes_before"] = resident
    return {
        "index": index, "evicted": args.evict, "wall_seconds": round(wall, 6),
        "logical_bytes_read": sum(row["size_bytes"] for row in rows
                                  if row["outcome"] == "copied"),
        "hash_set_sha256": observe.hash_set_sha256(hashes),
        "process": usage, "host_counters": counters, "files": rows,
    }


def capture(args: argparse.Namespace) -> dict[str, Any]:
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    root = Path(manifest["root"])
    check_scratch(args.scratch_dir, root)
    selected = baseline.select_files(manifest, args.ids)
    fs_type = baseline.corpus_filesystem(root, selected)
    example = args.example_dir / "copy-commit"
    build = observe.build_identity(example)
    build["harness_sha256"] = baseline.sha256_file(Path(__file__).resolve())
    files, skipped = observe.within_budget(selected, args.max_bytes)
    if not files:
        raise HarnessError("no selected file fits --max-bytes")
    repetitions = [run_repetition(index, root, files, args, fs_type, example)
                   for index in range(1, args.repetitions + 1)]
    return {
        "schema": SCHEMA,
        "corpus": {
            "label": manifest["label"], "filesystem_type": fs_type,
            "storage_class": baseline.storage_class(fs_type),
            "manifest_files": len(manifest["files"]),
            "selected_ids": [entry["id"] for entry in files], "skipped_budget_ids": skipped,
        },
        "build": build,
        "host": baseline.host_identity(),
        "parameters": {
            "path": PATH, "copy_buffer_bytes": buffer_bytes(), "concurrency": CONCURRENCY,
            "max_bytes_per_repetition": args.max_bytes, "repetitions": args.repetitions,
            "evict": args.evict,
        },
        "repetitions": repetitions,
        "not_measured": NOT_MEASURED,
    }


REQUIRED_KEYS = {
    **observe.REQUIRED_KEYS,
    "parameters": {"path", "copy_buffer_bytes", "concurrency", "max_bytes_per_repetition",
                   "repetitions", "evict"},
}
PROCESS_KEYS = {"cpu_seconds", "peak_rss_bytes", *IO_COUNTERS}
ROW_KEYS = observe.ROW_KEYS - {"observe_seconds"} | {"copy_seconds"}


def row_problems(row: dict[str, Any]) -> list[str]:
    problems = baseline.missing("file", row, ROW_KEYS)
    if not re.fullmatch(r"f\d{4}", str(row.get("id", ""))):
        problems.append("file id")
    for key in ("size_bytes", "queue_delay_seconds", "copy_seconds", "resident_bytes_before"):
        if not baseline.is_metric(row.get(key)):
            problems.append(f"file {key}")
    outcome, failure = row.get("outcome"), row.get("failure")
    if outcome not in OUTCOMES:
        problems.append("file outcome")
    failed_ok = isinstance(failure, str) and re.fullmatch(r"[a-z_]{1,64}", failure)
    if (outcome == "copied" and failure is not None) or \
            (outcome == "copy_failed" and not failed_ok):
        problems.append("file failure")
    return problems


def repetition_problems(repetition: dict[str, Any]) -> list[str]:
    problems = baseline.missing("repetition", repetition, observe.REPETITION_KEYS)
    for key in ("wall_seconds", "logical_bytes_read"):
        if not baseline.is_metric(repetition.get(key)):
            problems.append(f"repetition {key}")
    if not re.fullmatch(r"[0-9a-f]{64}", str(repetition.get("hash_set_sha256", ""))):
        problems.append("repetition hash_set_sha256")
    process = repetition.get("process") or {}
    problems += baseline.missing("process", process, PROCESS_KEYS)
    problems += [f"process {key}" for key, value in process.items()
                 if not baseline.is_metric(value)]
    counters = repetition.get("host_counters") or {}
    problems += baseline.missing("host_counters", counters, observe.COUNTER_KEYS)
    problems += ["host counter" for value in counters.values() if not baseline.is_metric(value)]
    rows = repetition.get("files") or []
    if not rows:
        problems.append("repetition files")
    for row in rows:
        problems += row_problems(row)
    return problems


def validate_run(record: dict[str, Any]) -> list[str]:
    """Return every contract violation in a committed-safe copy record."""
    problems = [] if record.get("schema") == SCHEMA else ["schema"]
    for section in ("corpus", "build", "host", "parameters", "repetitions", "not_measured"):
        if section not in record:
            problems.append(f"missing {section}")
    if problems:
        return problems
    for section, keys in REQUIRED_KEYS.items():
        problems += baseline.missing(section, record[section], keys)
    if record["parameters"].get("path") != PATH:
        problems.append("parameters path")
    if set(record["not_measured"]) != set(NOT_MEASURED):
        problems.append("not_measured")
    if not record["repetitions"]:
        problems.append("repetitions")
    for repetition in record["repetitions"]:
        problems += repetition_problems(repetition)
    text = json.dumps(record)
    if re.search(r'"/|\\\\|/home/|/Users/|/mnt/', text):
        problems.append("host path")
    if "blake3:" in text:
        problems.append("per-file hash")
    return problems


# ---- comparison ------------------------------------------------------------------------


def repetition_metrics(repetition: dict[str, Any]) -> dict[str, Any]:
    process = repetition["process"]
    return {
        "wall": repetition["wall_seconds"],
        "copy_seconds": sum(row["copy_seconds"] for row in repetition["files"]),
        "cpu": process["cpu_seconds"],
        "physical_read_bytes": process["read_bytes"],
        "physical_write_bytes": process["write_bytes"],
        "peak_rss": process["peak_rss_bytes"],
        "logical_bytes": repetition["logical_bytes_read"],
    }


def series(record: dict[str, Any]) -> tuple[dict[str, list], dict[str, Any] | None]:
    """Values over the compared repetitions; `as-found` repetition 1 is reported apart."""
    repetitions, first = record["repetitions"], None
    if not record["parameters"]["evict"]:
        first, repetitions = repetition_metrics(repetitions[0]), repetitions[1:]
    values: dict[str, list] = {}
    for repetition in repetitions:
        for name, value in repetition_metrics(repetition).items():
            values.setdefault(name, []).append(value)
    return values, first


def refuse_mismatch(base: dict[str, Any], cand: dict[str, Any]) -> None:
    for section, key in (("corpus", "label"), ("corpus", "selected_ids"),
                         ("parameters", "evict")):
        if base[section][key] != cand[section][key]:
            raise HarnessError(f"cells differ in {section} {key}; they do not compare")
    if base["parameters"]["copy_buffer_bytes"] == cand["parameters"]["copy_buffer_bytes"]:
        raise HarnessError("cells use the same buffer; nothing to compare")


def verdict(metrics: dict[str, dict[str, Any]], breached: list[str], failed: bool,
            hashes_equal: bool) -> str:
    """The spec's rule: #838's rule with summed copy seconds in place of observe seconds."""
    judged = [metrics[name]["verdict"] for name in ("cpu", "wall", "copy_seconds")]
    if failed or not hashes_equal or breached or "regression" in judged:
        return "no-go"
    if metrics["cpu"]["verdict"] == "improvement" and UNAVAILABLE not in judged:
        return "go"
    return "inconclusive"


def compare(base: dict[str, Any], cand: dict[str, Any]) -> dict[str, Any]:
    refuse_mismatch(base, cand)
    base_values, base_first = series(base)
    cand_values, cand_first = series(cand)
    if not base_values or not cand_values:
        raise HarnessError("a cell has no compared repetition; as-found cells need two or more")
    metrics = {name: candidates.classify(base_values[name], cand_values[name])
               for name in base_values}
    if cand_first is not None and base_first is not None:
        for name, value in cand_first.items():
            metrics[name].update(baseline_first=base_first[name], candidate_first=value)
    breached = observe.guardrails(base_values, cand_values)
    failed = any(row["outcome"] != "copied"
                 for record in (base, cand)
                 for repetition in record["repetitions"] for row in repetition["files"])
    hashes_equal = len(observe.hash_sets(base) | observe.hash_sets(cand)) == 1
    return {"metrics": metrics, "guardrails": breached, "failed": failed,
            "hashes_equal": hashes_equal,
            "verdict": verdict(metrics, breached, failed, hashes_equal)}


def render(base: dict[str, Any], cand: dict[str, Any], result: dict[str, Any]) -> list[str]:
    cache = "evicted" if cand["parameters"]["evict"] else "as-found"
    lines = [f"### {PATH}, {cache}: {base['parameters']['copy_buffer_bytes']} vs "
             f"{cand['parameters']['copy_buffer_bytes']} bytes", "",
             "| Metric | Baseline median | Candidate median | Baseline CV | Verdict |",
             "|---|---:|---:|---:|---|"]
    for metric, value in result["metrics"].items():
        cv = value.get("baseline_cv")
        lines.append(
            f"| {metric} | {value.get('baseline_median', UNAVAILABLE)} | "
            f"{value.get('candidate_median', UNAVAILABLE)} | "
            f"{UNAVAILABLE if cv is None else f'{cv:.1%}'} | {value['verdict']} |")
        if "candidate_first" in value:
            lines.append(f"| {metric}, repetition 1 | {value['baseline_first']} | "
                         f"{value['candidate_first']} | | reported separately |")
    lines += ["", f"Guardrails breached: {', '.join(result['guardrails']) or 'none'}. "
              f"Failed rows: {'yes' if result['failed'] else 'none'}. "
              f"Hash sets equal: {'yes' if result['hashes_equal'] else 'no'}. "
              f"Verdict: **{result['verdict']}**.", ""]
    return lines


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    run = commands.add_parser("run", help="copy a manifest's files through the commit loop")
    run.add_argument("manifest", type=Path)
    run.add_argument("--scratch-dir", type=Path, required=True,
                     help="empty directory on the corpus's filesystem, outside the corpus")
    run.add_argument("--example-dir", type=Path,
                     default=REPO_ROOT / "target" / "release" / "examples")
    run.add_argument("--output", type=Path, required=True)
    run.add_argument("--ids", help="comma-separated manifest ids; default is every file")
    run.add_argument("--max-bytes", type=int, required=True,
                     help="logical-read budget per repetition (one copy per file)")
    run.add_argument("--repetitions", type=int, default=3)
    run.add_argument("--evict", action="store_true",
                     help="drop page cache for selected files first (local storage only)")
    judge = commands.add_parser("compare", help="judge a candidate buffer against a baseline")
    judge.add_argument("baseline", type=Path)
    judge.add_argument("candidate", type=Path)
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        if args.command == "run":
            record = capture(args)
            problems = validate_run(record)
            if problems:
                raise HarnessError("run record failed validation: " + ", ".join(problems))
            baseline.write_new(args.output, record, 0o644)
        else:
            base, cand = (json.loads(path.read_text(encoding="utf-8"))
                          for path in (args.baseline, args.candidate))
            print("\n".join(render(base, cand, compare(base, cand))))
    except (HarnessError, OSError, ValueError, KeyError) as error:
        print(f"media-evidence-copy: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
