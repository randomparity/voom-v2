#!/usr/bin/env python3
"""Prove the #796 fingerprint experiment measures what it claims and commits nothing private.

The oracle runs on synthetic fixtures in a scratch directory, so it runs on any CI host.
Real-corpus evidence is the committed records, which are validated here too.
"""

import ast
import contextlib
import importlib.util
import io
import json
import os
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

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


class RandomPlanTests(unittest.TestCase):
    """#798: seeded-random interior blocks, modelling a check against a stored block table."""

    def interior_bytes(self, candidate, size, seed):
        """Bytes read between the 64 KiB head and tail; adjacent blocks merge into one range."""
        plan = experiment.sample_plan(candidate, size, seed)
        self.assertEqual(plan[0][0], 0)
        self.assertEqual(plan[-1][1], size)
        for start, end in plan:
            self.assertEqual(start % (64 * KIB), 0)
            self.assertTrue(end % (64 * KIB) == 0 or end == size)
        return sum(end - start for start, end in plan) - 2 * 64 * KIB

    def test_blocks_are_aligned_distinct_and_inside_the_interior_pool(self):
        self.assertEqual(self.interior_bytes("rnd64-i32", 8 * MIB, seed=7), 32 * 64 * KIB)
        self.assertEqual(self.interior_bytes("rnd64-i8", 64 * MIB, seed=7), 8 * 64 * KIB)

    def test_seed_reproduces_the_plan_and_another_seed_moves_it(self):
        size = 64 * MIB
        self.assertEqual(experiment.sample_plan("rnd64-i8", size, 1),
                         experiment.sample_plan("rnd64-i8", size, 1))
        self.assertNotEqual(experiment.sample_plan("rnd64-i8", size, 1),
                            experiment.sample_plan("rnd64-i8", size, 2))

    def test_sample_count_is_clamped_to_the_interior_pool(self):
        self.assertEqual(experiment.interior_block_count(10 * 64 * KIB), 8)
        self.assertEqual(self.interior_bytes("rnd64-i32", 10 * 64 * KIB, seed=3), 8 * 64 * KIB)
        self.assertEqual(experiment.sample_plan("rnd64-i8", 100 * KIB, 3), [(0, 100 * KIB)])

    def test_parameters_name_ctime_and_sampling_kind(self):
        parameters = experiment.candidate_parameters(["ht64-i8", "rnd64-i8", "meta-ctime"])
        self.assertEqual(parameters["ht64-i8"]["sampling"], "fixed")
        self.assertEqual(parameters["rnd64-i8"]["sampling"], "random")
        self.assertEqual(parameters["meta-ctime"]["sampling"], "none")
        self.assertTrue(parameters["meta-ctime"]["ctime"])
        self.assertFalse(parameters["ht64-i8"]["ctime"])


class FingerprintTests(unittest.TestCase):
    def test_only_meta_ctime_sees_a_metadata_only_change(self):
        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch, "chmod.bin")
            write_file(path, 1 * MIB)
            before = {name: experiment.fingerprint(path, name)[0]
                      for name in ("meta", "meta-ctime", "ht64")}
            time.sleep(experiment.CLOCK_TICK_SECONDS)
            os.chmod(path, 0o600)
            for name, value in before.items():
                changed = experiment.fingerprint(path, name)[0] != value
                self.assertEqual(changed, name == "meta-ctime", name)
            self.assertEqual(experiment.fingerprint(path, "meta-ctime")[1], 0)

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
        self.assertEqual(
            fields, {"candidate", "size", "mtime_ns", "ctime_ns", "file_key", "sample_digest"}
        )
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
        self.assertEqual(self.outcomes["O10", "meta"], "missed")
        self.assertEqual(self.outcomes["O10", "ht64"], "detected")
        self.assertEqual(self.outcomes["O12", "meta"], "missed")

    def test_reuse_candidates_have_valid_outcomes_and_ctime_catches_restored_mtime(self):
        with tempfile.TemporaryDirectory() as parent:
            record = experiment.run_oracle(
                Path(parent), candidates=list(experiment.REUSE_CANDIDATES))
        self.assertEqual(experiment.validate_oracle(record), [])
        outcomes = {(row["case"], row["candidate"]): row["outcome"] for row in record["cases"]}
        self.assertNotIn("invalid", outcomes.values())
        # The default wait spans a coarse tick, so any CI kernel's ctime moves.
        for case in ("O1", "O10", "O13", "O12"):
            self.assertEqual(outcomes[case, "meta-ctime"], "detected")
        for candidate in ("rnd64-i8", "rnd64-i32"):
            self.assertEqual(outcomes["O1", candidate], "missed")
            self.assertEqual(outcomes["O13", candidate], "missed")
            self.assertEqual(outcomes["O10", candidate], "detected")
            self.assertEqual(outcomes["O8", candidate], "equal")

    def test_scratch_directory_is_removed(self):
        self.assertEqual(list(Path(self.parent.name).iterdir()), [])

    def test_v2_rows_carry_the_tick_witness(self):
        with tempfile.TemporaryDirectory() as parent:
            record = experiment.run_oracle(Path(parent), candidates=["meta", "meta-ctime"],
                                           tick_wait=0)
        self.assertEqual(record["schema"], experiment.ORACLE_SCHEMA)
        self.assertEqual(record["parameters"]["tick_wait_seconds"], 0)
        self.assertEqual(experiment.validate_oracle(record), [])
        rows = {(row["case"], row["candidate"]): row for row in record["cases"]}
        self.assertTrue(all(row["same_coarse_tick"] in (True, False, None)
                            for row in rows.values()))
        on_linux = sys.platform.startswith("linux")
        self.assertEqual(experiment.COARSE_CLOCK, 5 if on_linux else None)
        timed = [row["same_coarse_tick"] for row in rows.values()]
        if on_linux:
            self.assertTrue(all(isinstance(value, bool) for value in timed))
        else:
            self.assertTrue(all(value is None for value in timed))
        # The control: size and mtime alone never see a mtime-restored edit, at any wait.
        self.assertEqual(rows["O10", "meta"]["outcome"], "missed")
        self.assertEqual(rows["O1", "meta"]["outcome"], "missed")
        self.assertNotIn("invalid", {row["outcome"] for row in rows.values()})

    def test_v3_records_carry_the_kernel_release(self):
        self.assertEqual(self.record["schema"], experiment.ORACLE_SCHEMA)
        self.assertEqual(self.record["parameters"]["kernel_release"],
                         experiment.platform.release())
        original = experiment.platform.release
        experiment.platform.release = lambda: ""
        try:
            self.assertEqual(experiment.kernel_release(), experiment.UNAVAILABLE)
        finally:
            experiment.platform.release = original

    def test_validator_requires_the_kernel_release_from_v3_only(self):
        for value in (None, "", 7):
            record = json.loads(json.dumps(self.record))
            record["parameters"]["kernel_release"] = value
            self.assertIn("kernel_release", experiment.validate_oracle(record))
        record = json.loads(json.dumps(self.record))
        del record["parameters"]["kernel_release"]
        self.assertIn("kernel_release", experiment.validate_oracle(record))
        record["schema"] = experiment.ORACLE_SCHEMA_V2
        self.assertEqual(experiment.validate_oracle(record), [])
        # /3 keeps every /2 check.
        record["schema"] = experiment.ORACLE_SCHEMA
        record["parameters"]["kernel_release"] = "6.1.0"
        record["parameters"]["tick_wait_seconds"] = -1
        record["cases"][0].pop("same_coarse_tick")
        problems = experiment.validate_oracle(record)
        self.assertIn("tick_wait_seconds", problems)
        self.assertIn("case same_coarse_tick", problems)

    def test_validator_accepts_v1_and_checks_the_v2_fields(self):
        v1 = json.loads(json.dumps(self.record))
        v1["schema"] = experiment.ORACLE_SCHEMA_V1
        del v1["parameters"]["kernel_release"]
        del v1["parameters"]["tick_wait_seconds"]
        for row in v1["cases"]:
            del row["same_coarse_tick"]
        self.assertEqual(experiment.validate_oracle(v1), [])
        for mutate in (lambda row: row.update(same_coarse_tick="yes"),
                       lambda row: row.pop("same_coarse_tick")):
            record = json.loads(json.dumps(self.record))
            mutate(record["cases"][0])
            self.assertIn("case same_coarse_tick", experiment.validate_oracle(record))
        for wait in (-1, True, "0"):
            record = json.loads(json.dumps(self.record))
            record["parameters"]["tick_wait_seconds"] = wait
            self.assertIn("tick_wait_seconds", experiment.validate_oracle(record))

    def test_negative_or_non_finite_tick_wait_is_refused(self):
        for wait in ("-0.1", "nan", "inf"):
            with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
                experiment.parse_args(["oracle", "--output", "x.json", "--tick-wait", wait])

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


class ProcessIoTests(unittest.TestCase):
    def process_io(self, proc):
        stderr = io.StringIO()
        with mock.patch.object(experiment.baseline, "PROC_ROOT", proc), \
                contextlib.redirect_stderr(stderr):
            counters = experiment.process_io()
        return counters, stderr.getvalue().splitlines()

    def test_readable_io_keeps_only_the_recorded_counters(self):
        with tempfile.TemporaryDirectory() as scratch:
            proc = Path(scratch)
            (proc / "self").mkdir()
            (proc / "self" / "io").write_text("rchar: 7\nwchar: 3\nread_bytes: 4096\n")
            counters, lines = self.process_io(proc)
        self.assertEqual(counters, {"rchar": 7, "read_bytes": 4096})
        self.assertEqual(lines, [])

    def test_unreadable_io_is_unavailable_and_stderr_names_the_cause(self):
        with tempfile.TemporaryDirectory() as scratch:
            proc = Path(scratch)
            (proc / "self" / "io").mkdir(parents=True)
            counters, lines = self.process_io(proc)
        self.assertEqual(counters, dict.fromkeys(("rchar", "read_bytes"), experiment.UNAVAILABLE))
        self.assertEqual(lines, [f"media-evidence-fingerprint: process {counter} unavailable: "
                                 "io: IsADirectoryError" for counter in ("rchar", "read_bytes")])

    def test_a_host_without_proc_says_why_the_counters_are_missing(self):
        with tempfile.TemporaryDirectory() as scratch:
            counters, lines = self.process_io(Path(scratch) / "absent")
        self.assertEqual(counters, dict.fromkeys(("rchar", "read_bytes"), experiment.UNAVAILABLE))
        self.assertEqual(lines, [f"media-evidence-fingerprint: process {counter} unavailable: "
                                 "no /proc on this host" for counter in ("rchar", "read_bytes")])


class CommittedRecordTests(unittest.TestCase):
    def test_committed_records_validate_and_cover_both_corpora(self):
        records = [json.loads(path.read_text()) for path in sorted(RECORD_DIR.glob("*.json"))]
        oracles = [record for record in records if record["schema"] in experiment.ORACLE_SCHEMAS]
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


class HarnessNameTests(unittest.TestCase):
    """Every counter-helper call names this harness through its one constant (#915)."""

    HARNESS_ARG = {"host_counters": 0, "proc_usage": 1}

    def test_every_counter_helper_call_passes_the_harness_constant(self):
        self.assertEqual(experiment.HARNESS, "media-evidence-fingerprint")
        seen = set()
        for node in ast.walk(ast.parse(Path(experiment.__file__).read_text())):
            if not isinstance(node, ast.Call):
                continue
            func = node.func
            helper = func.attr if isinstance(func, ast.Attribute) else getattr(func, "id", None)
            if helper not in self.HARNESS_ARG:
                continue
            seen.add(helper)
            argument = node.args[self.HARNESS_ARG[helper]]
            with self.subTest(helper=helper, line=node.lineno):
                self.assertEqual((type(argument), getattr(argument, "id", None)),
                                 (ast.Name, "HARNESS"))
        self.assertEqual(seen, set(self.HARNESS_ARG))


if __name__ == "__main__":
    unittest.main()
