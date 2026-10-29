#!/usr/bin/env python3
"""Prove the #796 fingerprint experiment measures what it claims and commits nothing private.

The oracle runs on synthetic fixtures in a scratch directory, so it runs on any CI host.
Real-corpus evidence is the committed records, which are validated here too.
"""

import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "media_evidence_fingerprint", Path(__file__).with_name("media-evidence-fingerprint.py")
)
experiment = importlib.util.module_from_spec(spec)
# Dataclass processing looks its defining module up by name.
sys.modules[spec.name] = experiment
spec.loader.exec_module(experiment)

KIB = 1024
MIB = 1024 * KIB
RECORD_DIR = experiment.RECORD_DIR


def write_file(path: Path, size: int, mtime_ns: int = 1_600_000_000_000_000_000) -> None:
    path.write_bytes(bytes(index % 251 for index in range(size)))
    os.utime(path, ns=(mtime_ns, mtime_ns))


class SamplePlanTests(unittest.TestCase):
    def test_metadata_candidate_reads_nothing(self):
        self.assertEqual(experiment.sample_plan("meta", 8 * MIB), [])

    def test_empty_file_has_no_ranges(self):
        for candidate in experiment.CANDIDATES:
            self.assertEqual(experiment.sample_plan(candidate, 0), [])

    def test_short_file_merges_head_and_tail_into_one_range(self):
        self.assertEqual(experiment.sample_plan("ht64", 100 * KIB), [(0, 100 * KIB)])
        self.assertEqual(experiment.sample_plan("ht1m", 1536 * KIB), [(0, 1536 * KIB)])

    def test_overlapping_interior_samples_are_read_once(self):
        self.assertEqual(experiment.sample_plan("ht64-i8", 160 * KIB), [(0, 160 * KIB)])

    def test_large_file_reads_disjoint_head_tail_and_interior_samples(self):
        self.assertEqual(
            experiment.sample_plan("ht64", 8 * MIB), [(0, 64 * KIB), (8 * MIB - 64 * KIB, 8 * MIB)]
        )
        plan = experiment.sample_plan("ht64-i32", 8 * MIB)
        self.assertEqual(len(plan), 34)
        self.assertEqual(sum(end - start for start, end in plan), 34 * 64 * KIB)
        self.assertTrue(all(left[1] < right[0] for left, right in zip(plan, plan[1:])))


class FingerprintTests(unittest.TestCase):
    def test_stable_file_gives_equal_fingerprints_and_counts_bytes(self):
        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch, "stable.bin")
            write_file(path, 4 * MIB)
            first, read = experiment.fingerprint(path, "ht64-i8")
            second, _ = experiment.fingerprint(path, "ht64-i8")
            self.assertEqual(first, second)
            self.assertEqual(read, 10 * 64 * KIB)

    def test_size_changing_write_between_reads_is_drift(self):
        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch, "drift.bin")
            write_file(path, 4 * MIB)

            def append() -> None:
                with path.open("ab") as handle:
                    handle.write(b"x")

            with self.assertRaises(experiment.Drift):
                experiment.fingerprint(path, "ht64", between_reads=append)

    def test_fingerprint_offers_no_conversion_into_other_evidence(self):
        fields = {field.name for field in experiment.dataclasses.fields(experiment.Fingerprint)}
        self.assertEqual(fields, {"candidate", "size", "mtime_ns", "file_key", "sample_digest"})
        public = {name for name in vars(experiment.Fingerprint) if not name.startswith("_")}
        self.assertEqual(public, set())


class OracleTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.parent = tempfile.TemporaryDirectory()
        cls.record = experiment.run_oracle(Path(cls.parent.name))
        cls.outcomes = {
            (row["case"], row["candidate"]): row["outcome"] for row in cls.record["cases"]
        }

    @classmethod
    def tearDownClass(cls):
        cls.parent.cleanup()

    def test_every_case_and_candidate_has_one_valid_outcome(self):
        cases = {f"O{number}" for number in range(1, 14)}
        self.assertEqual(set(self.outcomes), {
            (case, candidate) for case in cases for candidate in experiment.CANDIDATES
        })
        self.assertNotIn("invalid", self.outcomes.values())
        self.assertEqual(experiment.validate_oracle(self.record), [])

    def test_constructed_outcomes_match_what_each_candidate_reads(self):
        self.assertEqual(self.outcomes["O1", "meta"], "missed")
        for candidate in ("ht64", "ht1m", "ht64-i8", "ht64-i32"):
            self.assertEqual(self.outcomes["O1", candidate], "missed")
            for case in ("O2", "O3", "O4"):
                self.assertEqual(self.outcomes[case, candidate], "detected")
        for candidate in experiment.CANDIDATES:
            self.assertEqual(self.outcomes["O8", candidate], "equal")
            self.assertEqual(self.outcomes["O9", candidate], "detected")
        self.assertEqual(self.outcomes["O10", "meta"], "not-applicable")
        self.assertEqual(self.outcomes["O10", "ht64"], "detected")
        self.assertEqual(self.outcomes["O12", "meta"], "missed")

    def test_scratch_directory_is_removed(self):
        self.assertEqual(list(Path(self.parent.name).iterdir()), [])

    def test_broken_precondition_is_invalid_not_an_outcome(self):
        original = experiment.set_mtime
        experiment.set_mtime = lambda path, mtime_ns: None
        try:
            with tempfile.TemporaryDirectory() as parent:
                record = experiment.run_oracle(Path(parent), candidates=["meta"])
        finally:
            experiment.set_mtime = original
        outcomes = {row["case"]: row["outcome"] for row in record["cases"]}
        self.assertEqual(outcomes["O7"], "invalid")


class RunTests(unittest.TestCase):
    def test_as_found_run_records_cost_without_private_detail(self):
        with tempfile.TemporaryDirectory() as scratch:
            corpus = Path(scratch, "corpus-root")
            corpus.mkdir()
            for index, size in enumerate((0, 100 * KIB, 3 * MIB)):
                write_file(corpus / f"private-name-{index}.mkv", size)
            before = {path.name: path.stat().st_mtime_ns for path in corpus.iterdir()}
            manifest = Path(scratch, "manifest.json")
            manifest.write_text(json.dumps(
                experiment.baseline.build_manifest(corpus, "fixture-corpus")
            ))
            output = Path(scratch, "run.json")
            status = experiment.main([
                "run", str(manifest), "--repetitions", "2", "--output", str(output),
            ])
            self.assertEqual(status, 0)
            text = output.read_text()
            record = json.loads(text)
            self.assertEqual(experiment.validate_run(record), [])
            self.assertNotIn(scratch, text)
            self.assertNotIn("private-name", text)
            after = {path.name: path.stat().st_mtime_ns for path in corpus.iterdir()}
            self.assertEqual(before, after)
            cells = {cell["candidate"]: cell for cell in record["cells"]}
            self.assertEqual(set(cells), set(experiment.CANDIDATES))
            self.assertEqual(cells["meta"]["repetitions"][0]["logical_bytes_read"], 0)
            self.assertEqual(
                cells["ht64"]["repetitions"][0]["logical_bytes_read"], 100 * KIB + 128 * KIB
            )
            self.assertEqual(cells["ht64"]["observed"], {
                "files": 3, "failed": 0, "stable": 3, "distinct_content": 3,
            })

    def test_distinctness_ignores_inode_and_mtime(self):
        with tempfile.TemporaryDirectory() as scratch:
            first, second = Path(scratch, "a.mkv"), Path(scratch, "b.mkv")
            write_file(first, 4 * MIB, mtime_ns=1)
            write_file(second, 4 * MIB, mtime_ns=2)
            for candidate in experiment.CANDIDATES:
                observed = [experiment.fingerprint(path, candidate)[0]
                            for path in (first, second)]
                counts = experiment.observed_counts([observed])
                self.assertEqual(counts["distinct_content"], 1, candidate)

    def test_digest_or_inode_fields_fail_validation(self):
        record = json.loads(json.dumps(self.minimal_record()))
        record["cells"][0]["repetitions"][0]["files"][0]["sample_digest"] = "ab" * 32
        problems = experiment.validate_run(record)
        self.assertIn("private field sample_digest", problems)
        record["cells"][0]["repetitions"][0]["files"][0].pop("sample_digest")
        record["cells"][0]["repetitions"][0]["files"][0]["note"] = "cd" * 32
        self.assertIn("digest-like value", experiment.validate_run(record))

    @staticmethod
    def minimal_record():
        with tempfile.TemporaryDirectory() as scratch:
            corpus = Path(scratch, "root")
            corpus.mkdir()
            write_file(corpus / "a.mkv", 1 * MIB)
            manifest = Path(scratch, "manifest.json")
            manifest.write_text(json.dumps(experiment.baseline.build_manifest(corpus, "fixture")))
            output = Path(scratch, "run.json")
            experiment.main([
                "run", str(manifest), "--repetitions", "1", "--candidates", "ht64",
                "--output", str(output),
            ])
            return json.loads(output.read_text())


class CorpusBoundTests(unittest.TestCase):
    def test_non_local_storage_refuses_eviction_and_a_fourth_file(self):
        for storage in ("network", "unknown", "unavailable"):
            with self.assertRaises(experiment.baseline.HarnessError):
                experiment.check_corpus_bounds(storage, 4, evict=False)
            with self.assertRaises(experiment.baseline.HarnessError):
                experiment.check_corpus_bounds(storage, 3, evict=True)
            experiment.check_corpus_bounds(storage, 3, evict=False)
        experiment.check_corpus_bounds("local", 34, evict=True)


class CommittedRecordTests(unittest.TestCase):
    def test_committed_records_validate_and_cover_both_corpora(self):
        records = [json.loads(path.read_text()) for path in sorted(RECORD_DIR.glob("*.json"))]
        oracles = [record for record in records if record["schema"] == experiment.ORACLE_SCHEMA]
        runs = [record for record in records if record["schema"] == experiment.RUN_SCHEMA]
        self.assertEqual(len(oracles) + len(runs), len(records))
        self.assertTrue(oracles)
        self.assertEqual(
            {record["corpus"]["storage_class"] for record in runs}, {"local", "network"}
        )
        for record in oracles:
            self.assertEqual(experiment.validate_oracle(record), [])
            self.assertNotIn("invalid", {row["outcome"] for row in record["cases"]})
        for record in runs:
            self.assertEqual(experiment.validate_run(record), [])


if __name__ == "__main__":
    unittest.main()
