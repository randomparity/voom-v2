#!/usr/bin/env python3
"""Prove the #789 candidate tooling applies variants exactly and judges by the frozen protocol.

Comparison fixtures are synthetic records in the #788 schema; the thresholds they exercise
are the ones docs/media-evidence-protocol.md froze before any candidate ran.
"""

import importlib.util
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "media_candidates", Path(__file__).with_name("media-evidence-candidates.py")
)
candidates = importlib.util.module_from_spec(spec)
spec.loader.exec_module(candidates)

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


class VariantEditTests(unittest.TestCase):
    def test_every_variant_applies_exactly_once_to_the_current_source(self):
        source = (candidates.REPO_ROOT / candidates.HASH_SOURCE).read_text(encoding="utf-8")
        for name, edits in candidates.VARIANTS.items():
            with self.subTest(variant=name):
                edited = candidates.apply_edits(source, edits)
                self.assertEqual(edited == source, name == "baseline")

    def test_buffer_variants_change_only_the_hash_worker_chunk_size(self):
        source = (candidates.REPO_ROOT / candidates.HASH_SOURCE).read_text(encoding="utf-8")
        edited = candidates.apply_edits(source, candidates.VARIANTS["buf1m"])
        self.assertIn("pub const HASH_CHUNK_BYTES: usize = 1024 * 1024;", edited)
        self.assertIn("vec![0_u8; HASH_CHUNK_BYTES]", edited)

    def test_drifted_source_fails_instead_of_building_the_wrong_variant(self):
        with self.assertRaises(candidates.CandidateError):
            candidates.apply_edits("fn main() {}\n", candidates.VARIANTS["buf64k"])
        old = candidates.VARIANTS["batch8k"][1][0]
        with self.assertRaises(candidates.CandidateError):
            candidates.apply_edits(old + old, candidates.VARIANTS["batch8k"])


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

    def test_failed_rows_count_their_completed_stages(self):
        timings = {"f0001": (1.0, 2.0, "unavailable")}
        model = candidates.model(record([10.0], completions=timings))
        self.assertEqual(model["barrier_first_durable_seconds"], 3.0)


class PrepareTests(unittest.TestCase):
    def test_prepare_refuses_an_existing_directory(self):
        with tempfile.TemporaryDirectory() as existing:
            with self.assertRaises(candidates.CandidateError):
                candidates.prepare("baseline", Path(existing))

    def test_prepare_fails_on_drifted_source_before_creating_a_worktree(self):
        with tempfile.TemporaryDirectory() as parent:
            target = Path(parent) / "variant"
            drifted = dict(candidates.VARIANTS, drifted=[("no such source text", "")])
            original, candidates.VARIANTS = candidates.VARIANTS, drifted
            self.addCleanup(setattr, candidates, "VARIANTS", original)
            with self.assertRaises(candidates.CandidateError):
                candidates.prepare("drifted", target)
            self.assertFalse(target.exists())

    def test_prepare_refuses_an_unknown_variant(self):
        with tempfile.TemporaryDirectory() as parent:
            with self.assertRaises(candidates.CandidateError):
                candidates.prepare("buf9g", Path(parent) / "variant")


if __name__ == "__main__":
    unittest.main()
