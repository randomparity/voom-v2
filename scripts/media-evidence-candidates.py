#!/usr/bin/env python3
"""Candidate tooling for the issue #789 read-efficiency and scheduling stages.

`prepare` builds a bench-only worker variant in a detached worktree of HEAD, so production
source and defaults never change; the #788 harness in that worktree then records the
variant's constants and changed paths. `compare` judges candidate run records against a
baseline record with the thresholds frozen in docs/media-evidence-protocol.md. `model`
derives first-durable-observation timings for publication strategies from per-file
completion times; its output is modelled, never measured.
"""

from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[1]
HASH_SOURCE = "crates/voom-hash-worker/src/hash.rs"
UNAVAILABLE = "unavailable"
MIB = 1024 * 1024

CHUNK_CONSTANT = "pub const HASH_CHUNK_BYTES: usize = 8 * 1024;"
STACK_BUFFER = "    let mut buffer = [0_u8; HASH_CHUNK_BYTES];\n    loop {\n        let count =\n"
HEAP_BUFFER = "    let mut buffer = vec![0_u8; HASH_CHUNK_BYTES];\n    loop {\n        let count =\n"
ASYNC_LOOP = """pub async fn read_hash(file: &mut tokio::fs::File) -> Result<String, HashWorkerError> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; HASH_CHUNK_BYTES];
    loop {
        let count =
            file.read(&mut buffer)
                .await
                .map_err(|err| HashWorkerError::ArtifactUnreadable {
                    message: format!("cannot read candidate file while hashing: {err}"),
                    payload: serde_json::json!({ "stage": "read" }),
                })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}
"""
BLOCKING_LOOP = """pub async fn read_hash(file: &mut tokio::fs::File) -> Result<String, HashWorkerError> {
    let unreadable = |err: std::io::Error| HashWorkerError::ArtifactUnreadable {
        message: format!("cannot read candidate file while hashing: {err}"),
        payload: serde_json::json!({ "stage": "read" }),
    };
    let mut blocking = file.try_clone().await.map_err(unreadable)?.into_std().await;
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0_u8; HASH_CHUNK_BYTES];
        loop {
            let count = blocking.read(&mut buffer).map_err(unreadable)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        Ok(hasher.finalize().to_hex().to_string())
    })
    .await
    .map_err(|err| unreadable(std::io::Error::other(err)))?
}
"""


def buffer_variant(size: str) -> list[tuple[str, str]]:
    """A larger hash-worker chunk; heap-allocated so a multi-MiB buffer never sits on a stack."""
    return [
        (CHUNK_CONSTANT, f"pub const HASH_CHUNK_BYTES: usize = {size};"),
        (STACK_BUFFER, HEAP_BUFFER),
    ]


# Each variant is a list of (old, new) edits to the hash worker source; every old text must
# occur exactly once, so a drifted source fails rather than building something unmeasured.
VARIANTS: dict[str, list[tuple[str, str]]] = {
    "baseline": [],
    "buf64k": buffer_variant("64 * 1024"),
    "buf1m": buffer_variant("1024 * 1024"),
    "buf4m": buffer_variant("4 * 1024 * 1024"),
    "batch8k": [("use tokio::io::AsyncReadExt;\n", ""), (ASYNC_LOOP, BLOCKING_LOOP)],
}

# Lower is better for every compared metric. Resource metrics are judged by the protocol's
# guardrails for selection, not by the speed thresholds.
SPEED_METRICS = ("wall", "hash_seconds", "first_file_done", "physical_bytes")
RESOURCE_METRICS = ("hash_cpu", "probe_cpu", "hash_peak_rss", "probe_peak_rss")
FLUSH_COUNTS = (1, 10, 1000)


class CandidateError(Exception):
    """An operator-actionable refusal."""


def apply_edits(source: str, edits: list[tuple[str, str]]) -> str:
    for old, new in edits:
        count = source.count(old)
        if count != 1:
            raise CandidateError(
                f"variant edit expects one match in {HASH_SOURCE}, found {count}; "
                "update the variant for the current source"
            )
        source = source.replace(old, new)
    return source


def prepare(variant: str, directory: Path) -> Path:
    """Build `variant` in a detached worktree at `directory`; return its worker directory."""
    if variant not in VARIANTS:
        raise CandidateError(f"unknown variant {variant}; choose one of {', '.join(VARIANTS)}")
    if directory.exists():
        raise CandidateError(f"{directory} already exists; choose a new directory")
    # Edit in memory first, so a drifted source fails before any worktree exists.
    head = subprocess.run(["git", "-C", str(REPO_ROOT), "show", f"HEAD:{HASH_SOURCE}"],
                          capture_output=True, text=True, check=True).stdout
    edited = apply_edits(head, VARIANTS[variant])
    subprocess.run(["git", "-C", str(REPO_ROOT), "worktree", "add", "--detach", str(directory),
                    "HEAD"], check=True)
    try:
        (directory / HASH_SOURCE).write_text(edited, encoding="utf-8")
        subprocess.run(["cargo", "build", "--release", "--locked", "-p", "voom-hash-worker",
                        "-p", "voom-ffprobe-worker"], cwd=directory, check=True)
    except (OSError, subprocess.CalledProcessError):
        subprocess.run(["git", "-C", str(REPO_ROOT), "worktree", "remove", "--force",
                        str(directory)], check=False)
        raise
    return directory / "target" / "release"


def completion(row: dict[str, Any]) -> float:
    stages = [row["queue_delay_seconds"], row["hash_seconds"], row["probe_seconds"]]
    return sum(value for value in stages if value != UNAVAILABLE)


def physical_bytes(record: dict[str, Any], repetition: dict[str, Any]) -> int | str:
    if record["corpus"]["storage_class"] == "network":
        return repetition["host_counters"]["cifs_bytes_read"]
    values = [worker["read_bytes"] for worker in repetition["workers"].values()]
    return UNAVAILABLE if UNAVAILABLE in values else sum(values)


def repetition_metrics(record: dict[str, Any], repetition: dict[str, Any]) -> dict[str, Any]:
    rows, workers = repetition["files"], repetition["workers"]
    return {
        "wall": repetition["wall_seconds"],
        "hash_seconds": sum(row["hash_seconds"] for row in rows
                            if row["hash_seconds"] != UNAVAILABLE),
        "first_file_done": min(completion(row) for row in rows),
        "physical_bytes": physical_bytes(record, repetition),
        "hash_cpu": workers["hash"]["cpu_seconds"],
        "probe_cpu": workers["probe"]["cpu_seconds"],
        "hash_peak_rss": workers["hash"]["peak_rss_bytes"],
        "probe_peak_rss": workers["probe"]["peak_rss_bytes"],
    }


def series(record: dict[str, Any]) -> tuple[dict[str, list], dict[str, Any] | None]:
    """Per-metric values over the compared repetitions, and repetition 1 when as-found.

    The protocol reports `as-found` repetition 1 separately from repetitions 2 and later.
    """
    repetitions = record["repetitions"]
    first = None
    if not record["parameters"]["evict"]:
        first, repetitions = repetition_metrics(record, repetitions[0]), repetitions[1:]
    values: dict[str, list] = {name: [] for name in SPEED_METRICS + RESOURCE_METRICS}
    for repetition in repetitions:
        for name, value in repetition_metrics(record, repetition).items():
            values[name].append(value)
    return values, first


def classify(baseline: list, candidate: list) -> dict[str, Any]:
    if UNAVAILABLE in baseline or UNAVAILABLE in candidate or not baseline or not candidate:
        return {"verdict": UNAVAILABLE}
    base_median, cand_median = statistics.median(baseline), statistics.median(candidate)
    cv = statistics.stdev(baseline) / statistics.mean(baseline) if len(baseline) > 1 else None
    result = {"baseline_median": base_median, "candidate_median": cand_median,
              "baseline_cv": cv, "verdict": "inconclusive"}
    if cv is None or cv > 0.20 or base_median == 0:
        return result
    change = (base_median - cand_median) / base_median
    if change >= max(0.10, 2 * cv) and max(candidate) <= base_median:
        result["verdict"] = "improvement"
    elif -change >= max(0.05, 2 * cv):
        result["verdict"] = "regression"
    return result


def guardrails(base: dict[str, list], cand: dict[str, list]) -> list[str]:
    breached = []
    for worker in ("hash", "probe"):
        rss_base, rss_cand = base[f"{worker}_peak_rss"], cand[f"{worker}_peak_rss"]
        if UNAVAILABLE not in rss_base + rss_cand and \
                max(rss_cand) > statistics.median(rss_base) + 256 * MIB:
            breached.append(f"{worker} peak memory")
        cpu_base, cpu_cand = base[f"{worker}_cpu"], cand[f"{worker}_cpu"]
        if UNAVAILABLE in cpu_base + cpu_cand:
            continue
        wall_gain = 1 - statistics.median(cand["wall"]) / statistics.median(base["wall"])
        if statistics.median(cpu_cand) > 1.5 * statistics.median(cpu_base) and wall_gain < 0.25:
            breached.append(f"{worker} cpu")
    return breached


def compare(baseline: dict[str, Any], candidate: dict[str, Any]) -> dict[str, Any]:
    for section, key in (("corpus", "label"), ("corpus", "selected_ids"),
                         ("parameters", "evict")):
        if baseline[section][key] != candidate[section][key]:
            raise CandidateError(f"cells differ in {section} {key}; they do not compare")
    base, base_first = series(baseline)
    cand, cand_first = series(candidate)
    metrics = {name: classify(base[name], cand[name]) for name in base}
    if cand_first is not None:
        for name, value in cand_first.items():
            metrics[name].update(baseline_first=base_first[name], candidate_first=value)
    failed = any(row["outcome"] != "observed"
                 for repetition in candidate["repetitions"] for row in repetition["files"])
    return {"metrics": metrics, "guardrails": guardrails(base, cand), "failed": failed}


def select(results: dict[str, dict[str, Any]]) -> str:
    """Stage-1 selection: best wall improvement, then best summed-hash improvement."""
    eligible = {
        name: result for name, result in results.items()
        if not result["failed"] and not result["guardrails"]
        and all(result["metrics"][metric]["verdict"] != "regression" for metric in SPEED_METRICS)
    }
    for metric in ("wall", "hash_seconds"):
        improved = {name: result["metrics"][metric]["candidate_median"]
                    for name, result in eligible.items()
                    if result["metrics"][metric]["verdict"] == "improvement"}
        if improved:
            return min(improved, key=improved.get)
    return "baseline"


def model(record: dict[str, Any]) -> dict[str, Any]:
    """Modelled first durable observation per publication strategy, median over repetitions.

    Barrier: ADR 0067/0077 publish identity only in the completion transaction, so every
    file becomes durable when the last one completes (completion-transaction time is not
    modelled). Per-file readiness is hypothetical and would need a superseding ADR; it
    never covers absence, which only a complete traversal can establish. Flush count k
    is when the first k-observation batch leaves the agent; batches are provisional.
    Like `compare`, an `as-found` record uses repetitions 2 and later.
    """
    barrier, first, middle = [], [], []
    batches: dict[int, list[float]] = {count: [] for count in FLUSH_COUNTS}
    repetitions = record["repetitions"]
    if not record["parameters"]["evict"]:
        repetitions = repetitions[1:]
    for repetition in repetitions:
        done = sorted(completion(row) for row in repetition["files"])
        barrier.append(done[-1])
        first.append(done[0])
        middle.append(statistics.median(done))
        for count in FLUSH_COUNTS:
            batches[count].append(done[min(count, len(done)) - 1])
    return {
        "label": "modelled",
        "first_durable_observation_measured": "inconclusive",
        "barrier_first_durable_seconds": statistics.median(barrier),
        "per_file_first_ready_seconds": statistics.median(first),
        "per_file_median_ready_seconds": statistics.median(middle),
        "first_batch_sent_seconds": {count: statistics.median(values)
                                     for count, values in batches.items()},
    }


def render_comparison(name: str, result: dict[str, Any]) -> list[str]:
    lines = [f"### {name}", "",
             "| Metric | Baseline median | Candidate median | Baseline CV | Verdict |",
             "|---|---:|---:|---:|---|"]
    for metric, value in result["metrics"].items():
        cv = value.get("baseline_cv")
        lines.append(
            f"| {metric} | {value.get('baseline_median', UNAVAILABLE)} | "
            f"{value.get('candidate_median', UNAVAILABLE)} | "
            f"{UNAVAILABLE if cv is None else f'{cv:.1%}'} | {value['verdict']} |"
        )
        if "candidate_first" in value:
            lines.append(f"| {metric}, repetition 1 | {value['baseline_first']} | "
                         f"{value['candidate_first']} | | reported separately |")
    lines += ["", f"Guardrails breached: {', '.join(result['guardrails']) or 'none'}. "
              f"Failed rows: {'yes' if result['failed'] else 'none'}.", ""]
    return lines


def load(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("prepare", help="build a variant in a detached worktree")
    build.add_argument("variant", choices=list(VARIANTS))
    build.add_argument("directory", type=Path)
    judge = commands.add_parser("compare", help="judge candidates against a baseline record")
    judge.add_argument("baseline", type=Path)
    judge.add_argument("candidates", nargs="+", metavar="NAME=RECORD")
    derive = commands.add_parser("model", help="model publication timings from records")
    derive.add_argument("records", nargs="+", type=Path)
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        if args.command == "prepare":
            print(prepare(args.variant, args.directory))
        elif args.command == "compare":
            baseline = load(args.baseline)
            results = {}
            for item in args.candidates:
                name, _, path = item.partition("=")
                results[name] = compare(baseline, load(Path(path)))
                print("\n".join(render_comparison(name, results[name])))
            print(f"Stage-1 selection rule: {select(results)}")
        else:
            for path in args.records:
                print(json.dumps({"record": path.name, **model(load(path))}, sort_keys=True))
    except (CandidateError, OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"media-evidence-candidates: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
