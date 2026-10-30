#!/usr/bin/env python3
"""Prove the #789/#801 candidate tooling applies variants exactly and judges by the frozen protocol.

Comparison fixtures are synthetic records in the #788 schema; the thresholds they exercise
are the ones docs/media-evidence-protocol.md froze before any candidate ran.
"""

import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest import mock


def load_script(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


candidates = load_script("media_candidates", "media-evidence-candidates.py")
harness = load_script("media_baseline", "media-evidence-baseline.py")
BASELINE_DIR = candidates.REPO_ROOT / "docs" / "media-evidence-baseline"
SINGLE_READ_DIR = BASELINE_DIR / "single-read"

MIB = 1024 * 1024


def record(walls, hash_seconds=None, cpu=10.0, rss=5 * MIB, evict=True, ids=("f0001",),
           outcome="observed", completions=None):
    """A minimal run record: one row per id, the same per-row timings in every repetition."""
    repetitions = []
    for index, wall in enumerate(walls, start=1):
        rows = []
        for position, file_id in enumerate(ids):
            queue, hashed, probed = (completions or {}).get(file_id, (0.0, 1.0, 2.0))
            if hash_seconds is not None:
                hashed = hash_seconds[index - 1]
            rows.append({
                "id": file_id, "size_bytes": 100 + position, "outcome": outcome,
                "queue_delay_seconds": queue, "hash_seconds": hashed, "probe_seconds": probed,
            })
        usage = {"cpu_seconds": cpu, "peak_rss_bytes": rss, "read_bytes": 300, "rchar": 300}
        repetitions.append({
            "index": index, "wall_seconds": wall, "files": rows,
            "workers": {"hash": dict(usage), "probe": dict(usage)},
            "host_counters": {"network_rx_bytes": 0, "cifs_bytes_read": "unavailable"},
        })
    return {
        "corpus": {"label": "local-corpus", "selected_ids": list(ids), "storage_class": "local"},
        "parameters": {"evict": evict, "concurrency": 4},
        "repetitions": repetitions,
    }


def current(path):
    return (candidates.REPO_ROOT / path).read_text(encoding="utf-8")


class VariantEditTests(unittest.TestCase):
    def test_every_variant_applies_exactly_once_to_the_current_sources(self):
        for name, files in candidates.VARIANTS.items():
            for path, edits in files.items():
                with self.subTest(variant=name, path=path):
                    self.assertNotEqual(candidates.apply_edits(path, current(path), edits),
                                        current(path))
        self.assertEqual(candidates.VARIANTS["baseline"], {})

    def test_buffer_variants_change_only_the_hash_worker_chunk_size(self):
        files = candidates.VARIANTS["buf8k"]
        self.assertEqual(list(files), [candidates.HASH_SOURCE])
        edited = candidates.apply_edits(candidates.HASH_SOURCE, current(candidates.HASH_SOURCE),
                                        files[candidates.HASH_SOURCE])
        self.assertIn("pub const HASH_CHUNK_BYTES: usize = 8 * 1024;", edited)
        self.assertIn("vec![0_u8; HASH_CHUNK_BYTES]", edited)

    def test_batch8k_runs_an_8k_stack_buffer_loop_in_one_blocking_task(self):
        edited = candidates.apply_edits(candidates.HASH_SOURCE, current(candidates.HASH_SOURCE),
                                        candidates.VARIANTS["batch8k"][candidates.HASH_SOURCE])
        self.assertIn("pub const HASH_CHUNK_BYTES: usize = 8 * 1024;", edited)
        self.assertIn("tokio::task::spawn_blocking", edited)
        self.assertIn("let mut buffer = [0_u8; HASH_CHUNK_BYTES];", edited)

    def test_single_read_edits_only_the_probe_to_drop_both_rereads(self):
        files = candidates.VARIANTS["single-read"]
        self.assertEqual(list(files), [candidates.PROBE_SOURCE])
        source = current(candidates.PROBE_SOURCE)
        self.assertEqual(source.count("observe_file_facts(&path)"), 2)
        edited = candidates.apply_edits(candidates.PROBE_SOURCE, source,
                                        files[candidates.PROBE_SOURCE])
        self.assertNotIn("observe_file_facts", edited)
        self.assertIn('stat_facts("pre_probe", &path, &request.expected)', edited)
        self.assertIn('stat_facts("post_probe", &path, &request.expected)', edited)
        self.assertIn("modified_at != expected.modified_at", edited)

    def test_drifted_source_fails_instead_of_building_the_wrong_variant(self):
        hash_edits = candidates.VARIANTS["buf64k"][candidates.HASH_SOURCE]
        with self.assertRaises(candidates.CandidateError):
            candidates.apply_edits(candidates.HASH_SOURCE, "fn main() {}\n", hash_edits)
        old = candidates.ASYNC_LOOP
        with self.assertRaises(candidates.CandidateError):
            candidates.apply_edits(candidates.HASH_SOURCE, old + old,
                                   candidates.VARIANTS["batch8k"][candidates.HASH_SOURCE])
        probe_edits = candidates.VARIANTS["single-read"][candidates.PROBE_SOURCE]
        with self.assertRaises(candidates.CandidateError):
            candidates.apply_edits(candidates.PROBE_SOURCE, "fn main() {}\n", probe_edits)


class CompareTests(unittest.TestCase):
    def verdict(self, baseline, candidate, metric):
        return candidates.compare(baseline, candidate)["metrics"][metric]["verdict"]

    def test_improvement_needs_ten_percent_and_a_worst_rep_no_worse_than_baseline(self):
        baseline = record([100.0, 100.0, 100.0])
        self.assertEqual(self.verdict(baseline, record([85.0, 86.0, 87.0]), "wall"),
                         "improvement")
        self.assertEqual(self.verdict(baseline, record([95.0, 95.0, 95.0]), "wall"),
                         "inconclusive")
        self.assertEqual(self.verdict(baseline, record([80.0, 80.0, 101.0]), "wall"),
                         "inconclusive")

    def test_regression_needs_five_percent(self):
        baseline = record([100.0, 100.0, 100.0])
        self.assertEqual(self.verdict(baseline, record([106.0, 106.0, 106.0]), "wall"),
                         "regression")
        self.assertEqual(self.verdict(baseline, record([104.0, 104.0, 104.0]), "wall"),
                         "inconclusive")

    def test_thresholds_widen_to_twice_the_baseline_cv_and_stop_above_twenty_percent(self):
        noisy = record([90.0, 100.0, 110.0])
        self.assertEqual(self.verdict(noisy, record([85.0, 85.0, 85.0]), "wall"),
                         "inconclusive")
        self.assertEqual(self.verdict(noisy, record([70.0, 70.0, 70.0]), "wall"),
                         "improvement")
        wild = record([75.0, 100.0, 125.0])
        self.assertEqual(self.verdict(wild, record([10.0, 10.0, 10.0]), "wall"), "inconclusive")

    def test_summed_hash_seconds_is_its_own_metric(self):
        baseline = record([100.0] * 3, hash_seconds=[10.0, 10.0, 10.0])
        candidate = record([100.0] * 3, hash_seconds=[5.0, 5.0, 5.0])
        self.assertEqual(self.verdict(baseline, candidate, "hash_seconds"), "improvement")
        self.assertEqual(self.verdict(baseline, candidate, "wall"), "inconclusive")

    def test_memory_guardrail_flags_growth_beyond_256_mib(self):
        result = candidates.compare(record([100.0] * 3),
                                    record([100.0] * 3, rss=5 * MIB + 257 * MIB))
        self.assertIn("hash peak memory", result["guardrails"])
        result = candidates.compare(record([100.0] * 3),
                                    record([100.0] * 3, rss=5 * MIB + 255 * MIB))
        self.assertEqual(result["guardrails"], [])

    def test_cpu_guardrail_is_waived_only_by_a_quarter_wall_improvement(self):
        result = candidates.compare(record([100.0] * 3), record([90.0] * 3, cpu=16.0))
        self.assertIn("hash cpu", result["guardrails"])
        result = candidates.compare(record([100.0] * 3), record([74.0] * 3, cpu=16.0))
        self.assertEqual(result["guardrails"], [])

    def test_a_non_observed_row_fails_the_candidate(self):
        result = candidates.compare(record([100.0] * 3), record([50.0] * 3, outcome="disagree"))
        self.assertTrue(result["failed"])

    def test_zero_baseline_and_missing_repetitions_are_reported_not_crashed(self):
        baseline, candidate = record([100.0] * 3, cpu=0.0), record([100.0] * 3, cpu=1.0)
        result = candidates.compare(baseline, candidate)
        self.assertEqual(result["metrics"]["hash_cpu"]["verdict"], "inconclusive")
        with self.assertRaises(candidates.CandidateError):
            candidates.compare(record([10.0], evict=False), record([10.0], evict=False))

    def test_mismatched_cells_are_refused(self):
        baseline = record([100.0] * 3)
        for other in (record([100.0] * 3, ids=("f0002",)), record([100.0] * 3, evict=False)):
            with self.assertRaises(candidates.CandidateError):
                candidates.compare(baseline, other)

    def test_as_found_cells_compare_repetitions_after_the_first(self):
        baseline = record([300.0, 100.0, 100.0], evict=False)
        candidate = record([10.0, 100.0, 100.0], evict=False)
        result = candidates.compare(baseline, candidate)
        self.assertEqual(result["metrics"]["wall"]["verdict"], "inconclusive")
        self.assertEqual(result["metrics"]["wall"]["candidate_first"], 10.0)

    def test_selection_prefers_wall_improvement_then_hash_and_skips_failed_or_breached(self):
        baseline = record([100.0] * 3, hash_seconds=[10.0] * 3)
        wall_win = record([80.0] * 3, hash_seconds=[9.0] * 3)
        hash_win = record([100.0] * 3, hash_seconds=[2.0] * 3)
        failed = record([50.0] * 3, outcome="hash_failed")
        greedy = record([60.0] * 3, rss=400 * MIB)
        results = {name: candidates.compare(baseline, value) for name, value in
                   {"wall": wall_win, "hash": hash_win, "failed": failed,
                    "greedy": greedy}.items()}
        self.assertEqual(candidates.select(results), "wall")
        del results["wall"]
        self.assertEqual(candidates.select(results), "hash")
        del results["hash"]
        self.assertEqual(candidates.select(results), "baseline")

    def test_memory_growth_within_the_guardrail_does_not_disqualify_a_variant(self):
        baseline = record([100.0] * 3, hash_seconds=[10.0] * 3)
        larger_buffer = record([100.0] * 3, hash_seconds=[2.0] * 3, rss=40 * MIB)
        result = candidates.compare(baseline, larger_buffer)
        self.assertEqual(result["metrics"]["hash_peak_rss"]["verdict"], "regression")
        self.assertEqual(candidates.select({"larger": result}), "larger")


class ModelTests(unittest.TestCase):
    def test_barrier_readiness_and_flush_times_come_from_per_file_completion(self):
        timings = {"f0001": (0.0, 1.0, 2.0), "f0002": (0.0, 4.0, 4.0), "f0003": (3.0, 1.0, 1.0)}
        model = candidates.model(record([10.0], ids=tuple(timings), completions=timings))
        self.assertEqual(model["label"], "modelled")
        self.assertEqual(model["first_durable_observation_measured"], "inconclusive")
        self.assertEqual(model["barrier_first_durable_seconds"], 8.0)
        self.assertEqual(model["per_file_first_ready_seconds"], 3.0)
        self.assertEqual(model["per_file_median_ready_seconds"], 5.0)
        self.assertEqual(model["first_batch_sent_seconds"], {1: 3.0, 10: 8.0, 1000: 8.0})

    def test_as_found_records_model_repetitions_after_the_first(self):
        as_found = record([60.0, 10.0], evict=False)
        as_found["repetitions"][0]["files"][0]["hash_seconds"] = 57.0
        model = candidates.model(as_found)
        self.assertEqual(model["barrier_first_durable_seconds"], 3.0)

    def test_failed_rows_hold_the_barrier_but_are_never_ready(self):
        timings = {"f0001": (0.0, 0.5, "unavailable"), "f0002": (1.0, 2.0, 3.0)}
        failed = record([10.0], ids=tuple(timings), completions=timings)
        failed["repetitions"][0]["files"][0]["outcome"] = "probe_failed"
        model = candidates.model(failed)
        self.assertEqual(model["barrier_first_durable_seconds"], 6.0)
        self.assertEqual(model["per_file_first_ready_seconds"], 6.0)
        self.assertEqual(model["first_batch_sent_seconds"][1], 0.5)
        timings = {"f0001": (1.0, 2.0, "unavailable")}
        model = candidates.model(record([10.0], completions=timings, outcome="hash_failed"))
        self.assertEqual(model["barrier_first_durable_seconds"], 3.0)
        self.assertEqual(model["per_file_first_ready_seconds"], "unavailable")


class PrepareTests(unittest.TestCase):
    def test_prepare_refuses_an_existing_directory(self):
        with tempfile.TemporaryDirectory() as existing:
            with self.assertRaises(candidates.CandidateError):
                candidates.prepare("baseline", Path(existing))

    def test_prepare_fails_on_drifted_source_before_creating_a_worktree(self):
        with tempfile.TemporaryDirectory() as parent:
            target = Path(parent) / "variant"
            drifted = dict(candidates.VARIANTS, drifted={
                candidates.HASH_SOURCE: candidates.VARIANTS["buf64k"][candidates.HASH_SOURCE],
                candidates.PROBE_SOURCE: [("no such source text", "")],
            })
            original, candidates.VARIANTS = candidates.VARIANTS, drifted
            self.addCleanup(setattr, candidates, "VARIANTS", original)
            commands = []
            real_run = candidates.subprocess.run

            def recording_run(command, *args, **kwargs):
                commands.append(command)
                if "worktree" in command:
                    self.fail("prepare created a worktree before every edit applied")
                return real_run(command, *args, **kwargs)

            with mock.patch.object(candidates.subprocess, "run", recording_run):
                with self.assertRaises(candidates.CandidateError):
                    candidates.prepare("drifted", target)
            self.assertEqual(len(commands), 2)
            self.assertFalse(target.exists())

    def test_prepare_refuses_an_unknown_variant(self):
        with tempfile.TemporaryDirectory() as parent:
            with self.assertRaises(candidates.CandidateError):
                candidates.prepare("buf9g", Path(parent) / "variant")


class SingleReadRecordTests(unittest.TestCase):
    """The #788 self-test reads only the top-level baseline directory, so check these here."""

    def test_committed_records_are_valid_runs_of_the_single_read_build(self):
        buf1m = candidates.load(BASELINE_DIR / "2026-10-29-stage1-buf1m-local-corpus.json")
        current = candidates.load(BASELINE_DIR / "2026-10-29-local-corpus-evicted.json")
        paths = sorted(SINGLE_READ_DIR.glob("*.json"))
        self.assertTrue(paths)
        for path in paths:
            with self.subTest(record=path.name):
                record = candidates.load(path)
                self.assertEqual(harness.validate_run(record), [])
                self.assertEqual(record["build"]["git_changed_paths"], [
                    "crates/voom-ffprobe-worker/src/handler.rs",
                    "crates/voom-hash-worker/src/hash.rs",
                ])
                # The buf1m hash binary with a rebuilt probe worker that reads no corpus bytes.
                self.assertEqual(record["build"]["hash_worker_sha256"],
                                 buf1m["build"]["hash_worker_sha256"])
                self.assertNotEqual(record["build"]["probe_worker_sha256"],
                                    current["build"]["probe_worker_sha256"])
                for repetition in record["repetitions"]:
                    corpus = sum(row["size_bytes"] for row in repetition["files"])
                    self.assertLess(repetition["workers"]["probe"]["rchar"], corpus / 100)


if __name__ == "__main__":
    unittest.main()
