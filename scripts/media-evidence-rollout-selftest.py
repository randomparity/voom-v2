#!/usr/bin/env python3
"""Prove the #794 rollout instrument measures what it claims and commits nothing private.

Synthetic checks use scratch directories and a scratch SQLite database shaped like the
stack's tables, so they run on any CI host without built workers. Real-storage evidence is
the committed records under docs/media-evidence-baseline/rollout/, validated here.
"""

import importlib.util
import json
import os
import sqlite3
import sys
import tempfile
import threading
import time
import tomllib
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "media_evidence_rollout", Path(__file__).with_name("media-evidence-rollout.py")
)
rollout = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = rollout
spec.loader.exec_module(rollout)

RECORDS = Path(__file__).resolve().parents[1] / "docs" / "media-evidence-baseline" / "rollout"
HASH_A = "blake3:" + "a" * 64
HASH_B = "blake3:" + "b" * 64


def scratch_database(path: Path) -> sqlite3.Connection:
    connection = sqlite3.connect(path)
    connection.executescript(
        "CREATE TABLE scan_sessions (id INTEGER PRIMARY KEY, storage_root_id INTEGER, "
        "status TEXT);"
        "CREATE TABLE scan_observations (scan_session_id INTEGER, "
        "provider_relative_locator TEXT, evidence_json TEXT);"
        "CREATE TABLE file_locations (id INTEGER PRIMARY KEY, storage_root_id INTEGER, "
        "address_state TEXT, retired_at TEXT);"
    )
    return connection


def evidence(content_hash: str, source: str | None = None) -> str:
    value = {"content_hash": content_hash, "size_bytes": 1}
    if source:
        value["hash_source"] = source
    return json.dumps(value)


class RootTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        files = []
        for index, relative in enumerate(["a/one.mkv", "b/two.mkv", "b/three.mkv"], start=1):
            files.append({"id": f"f{index:04d}", "relative": relative})
        self.manifest = {"root": str(self.root), "files": files}

    def test_full_pass_scans_the_corpus_root(self):
        self.assertEqual(rollout.scan_roots(self.manifest, self.manifest["files"]), [self.root])

    def test_subset_scans_each_file_directory(self):
        roots = rollout.scan_roots(self.manifest, self.manifest["files"][:1])
        self.assertEqual(roots, [self.root / "a"])

    def test_subset_refuses_a_directory_holding_unselected_files(self):
        # A shared directory would scan and publish files outside the recorded subset.
        with self.assertRaises(rollout.HarnessError):
            rollout.scan_roots(self.manifest, self.manifest["files"][1:2])


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.database = Path(self.temp.name) / "voom.db"
        self.locators = {(7, "a/one.mkv"): "f0001", (7, "b/two.mkv"): "f0002",
                         (7, "c/three.mkv"): "f0003"}

    def summary(self, rows):
        connection = scratch_database(self.database)
        connection.execute("INSERT INTO scan_sessions VALUES (1, 7, 'succeeded')")
        connection.executemany("INSERT INTO scan_observations VALUES (1, ?, ?)", rows)
        connection.commit()
        connection.close()
        return rollout.stored_evidence(self.database, [1], self.locators)

    def test_counts_reused_recomputed_and_evidence_less_rows(self):
        summary = self.summary([("a/one.mkv", evidence(HASH_A, "reused")),
                                ("b/two.mkv", evidence(HASH_B)), ("c/three.mkv", None)])
        self.assertEqual(summary["observations"], 3)
        self.assertEqual(summary["with_evidence"], 2)
        self.assertEqual(summary["reused"], 1)
        self.assertEqual(summary["recomputed_without_source"], 1)
        self.assertEqual(summary["other_source"], 0)

    def test_an_unknown_hash_source_is_counted_separately(self):
        # G8: a recomputed row must carry no source; any other value is a visible anomaly.
        summary = self.summary([("a/one.mkv", evidence(HASH_A, "recomputed"))])
        self.assertEqual(summary["other_source"], 1)
        self.assertEqual(summary["recomputed_without_source"], 0)

    def test_digest_tracks_content_hashes_not_row_order_or_source(self):
        first = self.summary([("a/one.mkv", evidence(HASH_A)), ("b/two.mkv", evidence(HASH_B))])
        self.database.unlink()
        reordered = self.summary([("b/two.mkv", evidence(HASH_B, "reused")),
                                  ("a/one.mkv", evidence(HASH_A))])
        self.database.unlink()
        changed = self.summary([("a/one.mkv", evidence(HASH_B)), ("b/two.mkv", evidence(HASH_B))])
        self.assertEqual(first["hash_set_sha256"], reordered["hash_set_sha256"])
        self.assertNotEqual(first["hash_set_sha256"], changed["hash_set_sha256"])
        self.assertNotIn(HASH_A, json.dumps(first))

    def test_rows_outside_the_selection_are_unmatched(self):
        summary = self.summary([("z/other.mkv", evidence(HASH_A))])
        self.assertEqual(summary["unmatched"], 1)
        self.assertEqual(summary["with_evidence"], 0)


class PublicationTimingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.database = Path(self.temp.name) / "voom.db"
        connection = scratch_database(self.database)
        connection.execute("INSERT INTO scan_sessions VALUES (1, 7, 'running')")
        connection.commit()
        connection.close()

    def later(self, delay, statements):
        def apply():
            time.sleep(delay)
            connection = sqlite3.connect(self.database)
            for statement in statements:
                connection.execute(statement)
            connection.commit()
            connection.close()
        thread = threading.Thread(target=apply)
        thread.start()
        self.addCleanup(thread.join)

    def test_first_durable_observation_is_the_first_published_location(self):
        self.later(0.3, ["INSERT INTO file_locations VALUES (1, 7, 'rooted', NULL)"])
        self.later(0.6, ["UPDATE scan_sessions SET status = 'succeeded'"])
        first, complete, states = rollout.wait_for_publication(
            self.database, [7], [1], time.monotonic(), 10, lambda: None)
        self.assertGreaterEqual(first, 0.3)
        self.assertLess(first, complete)
        self.assertEqual(states, {1: "succeeded"})

    def test_retired_and_other_root_locations_are_not_publication(self):
        self.later(0.1, ["INSERT INTO file_locations VALUES (1, 7, 'rooted', 'retired')",
                         "INSERT INTO file_locations VALUES (2, 8, 'rooted', NULL)"])
        self.later(0.3, ["UPDATE scan_sessions SET status = 'failed'"])
        first, _, states = rollout.wait_for_publication(
            self.database, [7], [1], time.monotonic(), 10, lambda: None)
        self.assertEqual(first, rollout.UNAVAILABLE)
        self.assertEqual(states, {1: "failed"})

    def test_a_dead_stack_stops_the_wait(self):
        def dead():
            raise RuntimeError("voom-node-agent exited")
        with self.assertRaises(RuntimeError):
            rollout.wait_for_publication(self.database, [7], [1], time.monotonic(), 10, dead)

    def test_a_session_that_never_ends_fails_loudly(self):
        with self.assertRaises(rollout.HarnessError):
            rollout.wait_for_publication(self.database, [7], [1], time.monotonic(), 0.2,
                                         lambda: None)


class StackTests(unittest.TestCase):
    def test_reuse_setting_is_a_top_level_key(self):
        with tempfile.TemporaryDirectory() as temp:
            tools = Path(temp) / "tools"
            tools.mkdir()
            env = {}
            for name in ("ffmpeg", "ffprobe", "mkvmerge"):
                (tools / name).touch()
                env[f"VOOM_{name.upper()}_BIN"] = str(tools / name)
            (Path(temp) / "input").mkdir()
            args = rollout.stack_args(Path(temp) / "work", [Path(temp) / "input"], tools)
            with patch.dict(os.environ, env):
                for reuse in (True, False):
                    stack = rollout.Stack.__new__(rollout.Stack)
                    stack.reuse, stack.args, stack.roots, stack.node_id = reuse, args, [], 3
                    stack.bin_dir = tools
                    parsed = tomllib.loads(stack.configuration(Path(temp) / "token", "http://x"))
                    self.assertIs(parsed["probe_hash_reuse"], reuse)
                    self.assertEqual(parsed["node_id"], 3)


class UsageTests(unittest.TestCase):
    def test_counters_are_deltas_and_peak_memory_is_the_end_mark(self):
        before = {"cpu_seconds": 1.0, "peak_rss_bytes": 10, "rchar": 100, "read_bytes": 5}
        after = {"cpu_seconds": 3.5, "peak_rss_bytes": 30, "rchar": 400, "read_bytes": 5}
        self.assertEqual(rollout.usage_delta(before, after),
                         {"cpu_seconds": 2.5, "peak_rss_bytes": 30, "rchar": 300,
                          "read_bytes": 0})

    def test_an_unavailable_counter_stays_unavailable(self):
        before = {"cpu_seconds": "unavailable", "peak_rss_bytes": 1, "rchar": 1, "read_bytes": 1}
        delta = rollout.usage_delta(before, dict(before, cpu_seconds=2.0))
        self.assertEqual(delta["cpu_seconds"], "unavailable")

    @unittest.skipUnless(Path("/proc/self/io").exists(), "needs Linux /proc")
    def test_reads_this_process(self):
        usage = rollout.process_usage(os.getpid())
        self.assertTrue(all(isinstance(value, (int, float)) for value in usage.values()))

    @unittest.skipUnless(Path("/proc/self/stat").exists(), "needs Linux /proc")
    def test_worker_discovery_requires_one_hash_and_one_probe_worker(self):
        with self.assertRaises(rollout.HarnessError):
            rollout.worker_pids(os.getpid())


class ConstructionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "media.bin"
        self.path.write_bytes(bytes(range(256)) * 64)
        os.utime(self.path, ns=(1_000_000_000, 2_000_000_000))

    def test_restored_edit_keeps_size_and_mtime_but_not_bytes_or_ctime(self):
        before, content = self.path.stat(), self.path.read_bytes()
        time.sleep(0.02)
        rollout.same_size_edit(self.path, restore_mtime=True)
        after = self.path.stat()
        self.assertEqual((after.st_size, after.st_mtime_ns), (before.st_size, before.st_mtime_ns))
        self.assertNotEqual(self.path.read_bytes(), content)
        self.assertNotEqual(after.st_ctime_ns, before.st_ctime_ns)

    def test_ordinary_write_moves_mtime(self):
        before = self.path.stat()
        rollout.ordinary_write(self.path)
        after = self.path.stat()
        self.assertEqual(after.st_size, before.st_size)
        self.assertNotEqual(after.st_mtime_ns, before.st_mtime_ns)

    def test_outcome_keeps_only_class_code_and_stage_marker(self):
        frame = {"kind": "error", "class": "ARTIFACT_CHECKSUM_MISMATCH", "code": "x",
                 "message": "/private/path", "payload": {"stage": "pre_probe_guard"}}
        result = rollout.outcome("probe", frame)
        self.assertEqual(result["marker"], "pre_probe_guard")
        self.assertNotIn("/private/path", json.dumps(result))


class RecordTests(unittest.TestCase):
    def scan_record(self):
        repetition = {
            "index": 1, "evicted": True, "resident_bytes_before": {"f0001": 0},
            "first_durable_observation_seconds": 1.0, "complete_seconds": 1.0,
            "session_status": ["succeeded"], "worker_restarted": False,
            "evidence": {key: 0 for key in rollout.EVIDENCE_KEYS},
            "workers": {name: {key: 1 for key in rollout.baseline.WORKER_KEYS}
                        for name in rollout.WORKER_PROGRAMS},
            "host_counters": {}, "corpus_unchanged": True,
        }
        record = {"schema": rollout.SCAN_SCHEMA, "repetitions": [repetition],
                  "not_measured": dict(rollout.NOT_MEASURED)}
        for section, keys in rollout.SCAN_KEYS.items():
            record[section] = dict.fromkeys(keys, "x")
        return record

    def test_a_complete_scan_record_validates(self):
        self.assertEqual(rollout.validate_scan(self.scan_record()), [])

    def test_a_host_path_is_refused(self):
        record = self.scan_record()
        record["corpus"]["label"] = "/home/someone/media"
        self.assertIn("host path", rollout.validate_scan(record))

    def test_a_missing_metric_is_refused(self):
        record = self.scan_record()
        del record["repetitions"][0]["first_durable_observation_seconds"]
        self.assertNotEqual(rollout.validate_scan(record), [])
        record = self.scan_record()
        record["repetitions"][0]["workers"]["probe"]["rchar"] = None
        self.assertIn("worker probe metric", rollout.validate_scan(record))

    def test_a_construct_record_needs_every_case_in_order(self):
        cases = [{"case": name, "stage": "probe", "result": "failure", "detected": True}
                 for name in rollout.CASES]
        record = {"schema": rollout.CONSTRUCT_SCHEMA, "cases": cases}
        self.assertEqual(rollout.validate_construct(record), [])
        record["cases"] = cases[1:]
        self.assertIn("cases", rollout.validate_construct(record))

    def test_committed_records_validate(self):
        validators = {rollout.SCAN_SCHEMA: rollout.validate_scan,
                      rollout.CONSTRUCT_SCHEMA: rollout.validate_construct}
        for path in sorted(RECORDS.glob("*.json")):
            record = json.loads(path.read_text(encoding="utf-8"))
            with self.subTest(path.name):
                self.assertIn(record.get("schema"), validators)
                self.assertEqual(validators[record["schema"]](record), [])


if __name__ == "__main__":
    unittest.main()
