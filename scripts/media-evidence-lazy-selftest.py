#!/usr/bin/env python3
"""Prove the lazy full-hash lifecycle prototype keeps its recovery and redaction contracts.

The recovery matrix runs on scratch fixtures, so every case is checked on any CI host; the
committed corpus records are validated here too.
"""

import argparse
import copy
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "media_evidence_lazy", Path(__file__).with_name("media-evidence-lazy.py")
)
lazy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lazy)

RECORD_DIR = lazy.baseline.REPO_ROOT / "docs" / "media-evidence-baseline" / "lazy"
MIB = 1 << 20


class MatrixTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.record = lazy.matrix_record()
        cls.cases = {case["case"]: case for case in cls.record["cases"]}

    def observed(self, name):
        return self.cases[name]["observed"]

    def test_every_case_meets_its_stated_expectation(self):
        self.assertEqual(lazy.validate_matrix(self.record), [])
        for case in self.record["cases"]:
            with self.subTest(case=case["case"]):
                self.assertEqual(case["observed"], case["expected"])

    def test_a_killed_verifier_publishes_no_digest_and_the_resumed_one_verifies(self):
        observed = self.observed("restart")
        self.assertTrue(observed["killed_mid_read"])
        self.assertEqual(observed["state"], "provisional")
        self.assertFalse(observed["digest_published"])
        self.assertEqual(observed["plan"], "held")
        self.assertEqual(observed["resumed_state"], "verified")
        self.assertEqual(self.cases["restart"]["bytes"]["wasted"], 2 * MIB)

    def test_cancel_and_exhausted_retry_leave_evidence_provisional_and_plans_held(self):
        for name in ("cancel", "retry-exhausted"):
            with self.subTest(case=name):
                self.assertEqual(self.observed(name)["state"], "provisional")
                self.assertEqual(self.observed(name)["plan"], "held")
        self.assertEqual(self.cases["retry-exhausted"]["bytes"]["wasted"], 2 * MIB)

    def test_a_retry_counts_both_reads(self):
        counted = self.cases["retry"]["bytes"]
        self.assertEqual((counted["wasted"], counted["full"]), (2 * MIB, lazy.FIXTURE_BYTES))

    def test_disagreement_revokes_plans_and_quarantines_outputs(self):
        observed = self.observed("late-disagreement-head")
        self.assertEqual((observed["state"], observed["plan"], observed["output"]),
                         ("disagreement", "revoked", "quarantined"))

    def test_fingerprint_invisible_edits_are_reported_as_misses_not_passes(self):
        for name in ("unseen-interior-edit-during-read", "late-disagreement-interior",
                     "stale-evidence-interior"):
            with self.subTest(case=name):
                self.assertEqual(self.observed(name)["oracle"], "missed")

    def test_aged_background_verification_prevents_starvation(self):
        by_policy = {entry["policy"]: entry for entry in self.record["starvation"]}
        self.assertEqual(by_policy["on-demand-only"]["background_provisional"], 8)
        self.assertEqual(by_policy["aged"]["background_verified"], 8)

    def test_matrix_results_are_deterministic(self):
        again = lazy.run_matrix()
        self.assertEqual(again["cases"], self.record["cases"])
        self.assertEqual(again["starvation"], self.record["starvation"])

    def test_matrix_validation_rejects_an_unmet_expectation(self):
        record = copy.deepcopy(self.record)
        record["cases"][0]["observed"]["state"] = "stale"
        self.assertIn("case clean expectation", lazy.validate_matrix(record))

    def test_matrix_record_is_redacted(self):
        self.assertEqual(lazy.common_problems(self.record), [])


class LifecycleTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.path = self.base / "fixture.bin"
        lazy.write_fixture(self.path, 7)
        self.lifecycle = lazy.Lifecycle(lazy.StateFile(self.base / "state.json"))

    def test_verify_outside_provisional_is_a_recorded_refusal(self):
        self.assertEqual(self.lifecycle.verify("f", self.path), ("refused", 0))
        self.lifecycle.observe("f", self.path)
        self.lifecycle.verify("f", self.path)
        self.assertEqual(self.lifecycle.verify("f", self.path), ("refused", 0))

    def test_a_changed_stat_before_verification_is_stale_without_a_read(self):
        self.lifecycle.observe("f", self.path)
        with self.path.open("ab") as handle:
            handle.write(b"x")
        self.assertEqual(self.lifecycle.verify("f", self.path), ("stale", 0))

    def test_a_legitimate_rewrite_goes_stale_then_provisional_never_disagreement(self):
        self.lifecycle.observe("f", self.path)
        self.lifecycle.verify("f", self.path)
        lazy.write_fixture(self.path, 8)
        self.lifecycle.observe("f", self.path)
        self.assertEqual(self.lifecycle.state("f"), "stale")
        self.lifecycle.observe("f", self.path)
        self.assertEqual(self.lifecycle.verify("f", self.path)[0], "verified")

    def test_fingerprint_cost_covers_short_overlapping_and_long_files(self):
        window = lazy.FINGERPRINT_WINDOW_BYTES
        for size, cost in ((0, 0), (10, 10), (window + 5, window + 5), (10 * window, 2 * window)):
            with self.subTest(size=size):
                self.assertEqual(lazy.fingerprint_cost(size), cost)
                path = self.base / f"size-{size}"
                path.write_bytes(b"z" * size)
                self.assertEqual(lazy.read_fingerprint(path)[1], cost)

    def test_full_pass_window_matches_the_fingerprint_window(self):
        for size in (10, lazy.FINGERPRINT_WINDOW_BYTES + 5, 3 * MIB + 17):
            with self.subTest(size=size):
                self.path.write_bytes(bytes(range(256)) * (size // 256) + b"q" * (size % 256))
                fingerprint, _ = lazy.read_fingerprint(self.path)
                _, window, read = lazy.full_pass(self.path, size, lazy.Hooks())
                self.assertEqual((window, read), (fingerprint["window"], size))


class RunTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.root = self.base / "secret-library-root"
        self.sizes = {"a/Alpha Title (2001).mkv": 3 * MIB + 1, "b/Beta.mp4": 100,
                      "c/Gamma.mkv": 70_000}
        for relative, size in self.sizes.items():
            path = self.root / relative
            path.parent.mkdir(parents=True)
            path.write_bytes(b"m" * size)
        manifest = lazy.baseline.build_manifest(self.root, "fixture-corpus")
        self.manifest = self.base / "manifest.json"
        self.manifest.write_text(json.dumps(manifest), encoding="utf-8")

    def capture(self, max_bytes=10 * MIB):
        return lazy.capture(argparse.Namespace(
            manifest=self.manifest, ids=None, max_bytes=max_bytes, repetitions=2,
            concurrency=None, evict=False,
        ))

    def test_run_counts_each_component_and_leaves_media_unchanged(self):
        before = {path: path.read_bytes() for path in self.root.rglob("*") if path.is_file()}
        record = self.capture()
        self.assertEqual(lazy.validate_run(record), [])
        self.assertNotIn(str(self.base), json.dumps(record))
        self.assertNotIn("Alpha", json.dumps(record))
        sizes = list(self.sizes.values())
        fingerprints = sum(lazy.fingerprint_cost(size) for size in sizes)
        for repetition in record["repetitions"]:
            self.assertEqual(repetition["logical_bytes"], {
                "owner_fingerprint": fingerprints, "probe_reobservation": 2 * fingerprints,
                "deferred_full_hash": sum(sizes), "wasted": 0,
                "total": 3 * fingerprints + sum(sizes),
            })
            self.assertEqual({row["state"] for row in repetition["files"]}, {"verified"})
        self.assertTrue(record["corpus"]["unchanged_after_run"])
        # Every host provides process CPU time; only /proc counters may be unavailable.
        for repetition in record["repetitions"]:
            self.assertIsInstance(repetition["process"]["cpu_seconds"], float)
        self.assertEqual(before, {path: path.read_bytes() for path in before})

    def test_budget_skips_files_by_lazy_cost(self):
        record = self.capture(max_bytes=3 * MIB + 1 + 3 * 2 * lazy.FINGERPRINT_WINDOW_BYTES + 400)
        self.assertEqual(record["corpus"]["selected_ids"], ["f0001", "f0002"])
        self.assertEqual(record["corpus"]["skipped_budget_ids"], ["f0003"])

    def test_validation_rejects_host_paths_digests_and_missing_metrics(self):
        record = self.capture()
        self.assertEqual(lazy.validate_run(record), [])
        for mutate, problem in (
            (lambda r: r["corpus"].update(label="/home/someone"), "host path"),
            (lambda r: r["build"].update(note="blake3:00"), "digest recorded"),
            (lambda r: r["repetitions"][0]["process"].update(rchar=None), "repetition metric"),
            (lambda r: r["repetitions"][0]["files"][0].pop("wasted_bytes"),
             "file wasted_bytes"),
            (lambda r: r["corpus"].update(unchanged_after_run=False), "corpus changed"),
        ):
            changed = copy.deepcopy(record)
            mutate(changed)
            self.assertIn(problem, lazy.validate_run(changed))


def reference(total, physical, label="c", ids=("f0001",), evict=True):
    return {
        "corpus": {"label": label, "manifest_files": 1, "selected_ids": list(ids),
                   "storage_class": "local"},
        "parameters": {"evict": evict},
        "repetitions": [{"logical_bytes_read": total, "host_counters": {},
                         "workers": {"hash": {"read_bytes": physical},
                                     "probe": {"read_bytes": 0}}} for _ in range(3)],
    }


def candidate(totals, physical, ids=("f0001",)):
    return {
        "corpus": {"label": "c", "manifest_files": 1, "selected_ids": list(ids),
                   "storage_class": "local"},
        "parameters": {"evict": True},
        "repetitions": [{"logical_bytes": {"total": total}, "host_counters": {},
                         "process": {"read_bytes": physical}} for total in totals],
    }


class CompareTests(unittest.TestCase):
    def test_a_large_saving_is_an_improvement_and_a_large_increase_a_regression(self):
        result = lazy.compare(candidate([100, 101, 99], 100), reference(300, 300))
        self.assertEqual(result["metrics"]["logical_bytes"]["verdict"], "improvement")
        result = lazy.compare(candidate([330, 331, 329], 330), reference(300, 300))
        self.assertEqual(result["metrics"]["physical_bytes"]["verdict"], "regression")

    def test_a_small_change_is_inconclusive(self):
        result = lazy.compare(candidate([290, 291, 289], 290), reference(300, 300))
        self.assertEqual(result["metrics"]["logical_bytes"]["verdict"], "inconclusive")

    def test_a_worst_repetition_above_the_baseline_median_is_not_an_improvement(self):
        result = lazy.compare(candidate([100, 100, 400], 100), reference(300, 300))
        self.assertEqual(result["metrics"]["logical_bytes"]["verdict"], "inconclusive")

    def test_as_found_records_report_repetition_one_separately(self):
        base, lazy_record = reference(300, 300, evict=False), candidate([900, 100, 100], 100)
        lazy_record["parameters"]["evict"] = False
        result = lazy.compare(lazy_record, base)
        self.assertEqual(result["repetitions_compared"], 2)
        self.assertEqual(result["metrics"]["logical_bytes"]["verdict"], "improvement")

    def test_records_of_different_file_sets_are_refused(self):
        with self.assertRaisesRegex(lazy.HarnessError, "selected_ids"):
            lazy.compare(candidate([1, 1, 1], 1, ids=("f0002",)), reference(3, 3))
        with self.assertRaisesRegex(lazy.HarnessError, "label"):
            lazy.compare(candidate([1, 1, 1], 1), reference(3, 3, label="other"))


class CommittedRecordTests(unittest.TestCase):
    def test_committed_lazy_records_satisfy_their_contract(self):
        records = sorted(RECORD_DIR.glob("*.json"))
        self.assertTrue(records, "no committed lazy records")
        for path in records:
            with self.subTest(record=path.name):
                record = json.loads(path.read_text(encoding="utf-8"))
                self.assertIn(record.get("schema"), (lazy.RUN_SCHEMA, lazy.MATRIX_SCHEMA))
                self.assertEqual(lazy.validate(record), [])


if __name__ == "__main__":
    unittest.main()
