#!/usr/bin/env python3
"""Prove the #798 measurements show what they claim and commit nothing private.

Synthetic runs use scratch directories, so they run on any CI host. Real-filesystem and
real-corpus evidence is the committed records under docs/media-evidence-baseline/reuse/,
which are validated here, including the fingerprint-schema records of the new candidates.
"""

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "media_evidence_reuse", Path(__file__).with_name("media-evidence-reuse.py")
)
reuse = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = reuse
spec.loader.exec_module(reuse)
fingerprint = reuse.fingerprint

KIB = 1024
MIB = 1024 * KIB
SCRIPTS = Path(__file__).resolve().parent
GUARD = SCRIPTS / "check-media-evidence-fingerprint-guard.sh"


class ProbabilityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.parent = tempfile.TemporaryDirectory()
        cls.record = reuse.run_probability(Path(cls.parent.name), trials=400,
                                           sizes=(4 * MIB,))

    @classmethod
    def tearDownClass(cls):
        cls.parent.cleanup()

    def test_observed_rate_tracks_the_sampled_fraction(self):
        self.assertEqual(reuse.validate_probability(self.record), [])
        cells = {cell["candidate"]: cell for cell in self.record["cells"]}
        self.assertEqual(cells["rnd64-i8"]["interior_blocks"], 62)
        self.assertEqual(cells["rnd64-i32"]["sampled_blocks"], 32)
        for cell in cells.values():
            self.assertTrue(cell["within_bound"], cell)
        # More sampled blocks must detect more often; a sampler that never detects fails here.
        self.assertGreater(cells["rnd64-i8"]["detected"], 0)
        self.assertGreater(cells["rnd64-i32"]["detected"], cells["rnd64-i8"]["detected"])

    def test_analytic_table_compounds_over_independent_checks(self):
        rows = {(row["sampled_fraction"], row["checks"]): row["detection"]
                for row in self.record["analytic"]}
        self.assertEqual(rows[0.1, 1], 0.1)
        self.assertAlmostEqual(rows[0.1, 10], 1 - 0.9 ** 10, places=6)

    def test_scratch_directory_is_removed(self):
        self.assertEqual(list(Path(self.parent.name).iterdir()), [])


class ProbeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scratch = tempfile.TemporaryDirectory()
        root = Path(cls.scratch.name)
        cls.corpus, cls.parent = root / "corpus", root / "probe-parent"
        cls.corpus.mkdir()
        cls.parent.mkdir()
        cls.manifest = root / "manifest.json"
        cls.manifest.write_text(json.dumps({"root": str(cls.corpus)}))
        cls.record = reuse.run_ctime_probe([cls.parent], [cls.manifest], settle=0.01)
        cls.steps = {step["step"]: step for step in cls.record["filesystems"][0]["steps"]}

    @classmethod
    def tearDownClass(cls):
        cls.scratch.cleanup()

    def test_record_validates_and_scratch_is_removed(self):
        self.assertEqual(reuse.validate_probe(self.record), [])
        filesystem = self.record["filesystems"][0]
        self.assertTrue(filesystem["scratch_removed"])
        self.assertEqual(list(self.parent.iterdir()), [])
        self.assertEqual(filesystem["bytes_written"], 64 * KIB + 2)

    def test_restored_mtime_still_moves_ctime_and_a_read_moves_nothing(self):
        restored = self.steps["write-restoring-mtime"]
        self.assertFalse(restored["mtime_changed"])
        self.assertTrue(restored["ctime_changed_settled"])
        self.assertFalse(self.steps["read"]["ctime_changed_settled"])
        self.assertTrue(self.steps["utime"]["ctime_changed_settled"])
        self.assertTrue(self.steps["chmod"]["ctime_changed_settled"])

    def test_scratch_parent_inside_a_corpus_is_refused(self):
        inside = self.corpus / "nested"
        inside.mkdir()
        try:
            with self.assertRaises(fingerprint.baseline.HarnessError):
                reuse.run_ctime_probe([inside], [self.manifest], settle=0.01)
            self.assertEqual(list(inside.iterdir()), [])
        finally:
            inside.rmdir()

    def test_private_values_fail_validation(self):
        for key in ("ctime_ns", "generation", "seed", "mount_source", "mount_options",
                    "ino", "path"):
            record = json.loads(json.dumps(self.record))
            record["filesystems"][0][key] = 1
            self.assertIn(f"private field {key}", reuse.validate_probe(record), key)
        record = json.loads(json.dumps(self.record))
        record["filesystems"][0]["filesystem_type"] = "/mnt/share"
        self.assertIn("host path", reuse.validate_probe(record))


class GuardTests(unittest.TestCase):
    """The #796 guard covers fingerprint paths only; run it over copies of the reuse files."""

    def guard(self, extra: str = "") -> int:
        with tempfile.TemporaryDirectory() as root:
            scripts = Path(root, "scripts")
            records = Path(root, "docs", "media-evidence-baseline", "fingerprint")
            scripts.mkdir()
            records.mkdir(parents=True)
            for path in SCRIPTS.glob("media-evidence-reuse*.py"):
                text = path.read_text() + extra
                Path(scripts, f"media-evidence-fingerprint-copy-{path.name}").write_text(text)
            for path in reuse.RECORD_DIR.glob("*.json"):
                shutil.copy(path, records / f"reuse-{path.name}")
            result = subprocess.run([str(GUARD)], env={**os.environ,
                                    "VOOM_FINGERPRINT_GUARD_ROOT": root},
                                    capture_output=True, text=True, check=False)
            return result.returncode

    def test_reuse_files_name_no_production_identity_evidence(self):
        self.assertEqual(self.guard(), 0)

    def test_a_production_name_in_a_reuse_file_fails(self):
        # Split so this file does not itself match the guard.
        self.assertEqual(self.guard("\n# content" + "_hash\n"), 1)


class CommittedRecordTests(unittest.TestCase):
    def test_committed_records_validate_and_cover_every_measurement(self):
        records = [json.loads(path.read_text()) for path in sorted(reuse.RECORD_DIR.glob("*.json"))]
        by_schema = {}
        for record in records:
            by_schema.setdefault(record["schema"], []).append(record)
        validators = {
            fingerprint.ORACLE_SCHEMA: fingerprint.validate_oracle,
            fingerprint.RUN_SCHEMA: fingerprint.validate_run,
            reuse.PROBABILITY_SCHEMA: reuse.validate_probability,
            reuse.PROBE_SCHEMA: reuse.validate_probe,
        }
        self.assertEqual(set(by_schema), set(validators))
        for schema, group in by_schema.items():
            for record in group:
                self.assertEqual(validators[schema](record), [], schema)
        oracles = by_schema[fingerprint.ORACLE_SCHEMA]
        self.assertEqual({record["parameters"]["scratch_filesystem_type"] for record in oracles},
                         {"tmpfs", "btrfs"})
        for record in oracles:
            self.assertEqual(set(record["parameters"]["candidates"]),
                             set(fingerprint.REUSE_CANDIDATES))
            self.assertNotIn("invalid", {row["outcome"] for row in record["cases"]})
        runs = by_schema[fingerprint.RUN_SCHEMA]
        self.assertEqual({record["corpus"]["storage_class"] for record in runs},
                         {"local", "network"})
        for record in by_schema[reuse.PROBABILITY_SCHEMA]:
            self.assertTrue(all(cell["within_bound"] for cell in record["cells"]))
        probes = by_schema[reuse.PROBE_SCHEMA]
        self.assertEqual({filesystem["storage_class"] for record in probes
                          for filesystem in record["filesystems"]}, {"local", "network"})


if __name__ == "__main__":
    unittest.main()
