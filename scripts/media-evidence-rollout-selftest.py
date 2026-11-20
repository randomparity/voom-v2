#!/usr/bin/env python3
"""Prove the #794 rollout instrument measures what it claims and commits nothing private.

Synthetic checks use scratch directories and a scratch SQLite database shaped like the
stack's tables, so they run on any CI host without built workers. Real-storage evidence is
the committed records under docs/media-evidence-baseline/rollout/ (#794) and
docs/media-evidence-baseline/per-file-publication/ (#841), validated here.
"""

import argparse
import importlib.util
import io
import json
import os
import sqlite3
import sys
import tempfile
import time
import tomllib
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "media_evidence_rollout", Path(__file__).with_name("media-evidence-rollout.py")
)
rollout = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = rollout
spec.loader.exec_module(rollout)

BASELINE = Path(__file__).resolve().parents[1] / "docs" / "media-evidence-baseline"
RECORD_DIRECTORIES = (BASELINE / "rollout", BASELINE / "per-file-publication")
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
        "CREATE TABLE scan_observation_batches (scan_session_id INTEGER, sequence INTEGER);"
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

    def test_a_null_hash_source_is_not_an_absent_one(self):
        summary = self.summary([("a/one.mkv", json.dumps({"content_hash": HASH_A,
                                                           "hash_source": None}))])
        self.assertEqual(summary["other_source"], 1)

    def test_a_database_path_with_uri_characters_opens_read_only(self):
        database = Path(self.temp.name) / "odd#dir?" / "voom.db"
        database.parent.mkdir()
        self.database = database
        self.summary([("a/one.mkv", evidence(HASH_A))])
        connection = rollout.read_only(database)
        with self.assertRaises(sqlite3.OperationalError):
            connection.execute("CREATE TABLE t (x)")
        connection.close()

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

    def scripted(self, writes):
        """Poll on a fake clock: each sleep advances it one interval and applies the next write.

        A write is seen by the next poll only and every elapsed time is a whole number of
        intervals, so no runner speed can merge two writes into one poll or one time.
        """
        database, pending = self.database, list(writes)

        class Clock:
            # Not zero, so a wait that stopped subtracting `started` would report wrong times.
            now = 1000.0

            def monotonic(self):
                return self.now

            def sleep(self, seconds):
                self.now += seconds
                if pending:
                    connection = sqlite3.connect(database)
                    for statement in pending.pop(0):
                        connection.execute(statement)
                    connection.commit()
                    connection.close()

        clock = Clock()
        with patch.object(rollout, "time", clock):
            return rollout.wait_for_publication(
                self.database, [7], [1], clock.now, 10, lambda: None)

    def test_first_durable_observation_is_the_first_published_location(self):
        first, complete, states, _ = self.scripted([
            [], ["INSERT INTO file_locations VALUES (1, 7, 'rooted', NULL)"],
            ["UPDATE scan_sessions SET status = 'succeeded'"]])
        self.assertEqual(first, round(2 * rollout.POLL_SECONDS, 3))
        self.assertEqual(complete, round(3 * rollout.POLL_SECONDS, 3))
        self.assertLess(first, complete)
        self.assertEqual(states, {1: "succeeded"})

    def test_retired_and_other_root_locations_are_not_publication(self):
        first, _, states, _ = self.scripted([
            ["INSERT INTO file_locations VALUES (1, 7, 'rooted', 'retired')",
             "INSERT INTO file_locations VALUES (2, 8, 'rooted', NULL)"],
            ["UPDATE scan_sessions SET status = 'failed'"]])
        self.assertEqual(first, rollout.UNAVAILABLE)
        self.assertEqual(states, {1: "failed"})

    def test_publication_at_completion_is_not_before_complete(self):
        # Completion publication: the location and the terminal status commit together.
        _, _, _, before = self.scripted([
            [], ["INSERT INTO file_locations VALUES (1, 7, 'rooted', NULL)",
                 "UPDATE scan_sessions SET status = 'succeeded'"]])
        self.assertEqual(before, 0)

    def test_publication_while_running_counts_before_complete(self):
        _, _, _, before = self.scripted([
            ["INSERT INTO file_locations VALUES (1, 7, 'rooted', NULL)"],
            ["INSERT INTO file_locations VALUES (2, 7, 'rooted', NULL)"],
            ["UPDATE scan_sessions SET status = 'succeeded'"]])
        self.assertEqual(before, 2)

    def test_batches_count_only_the_selected_sessions(self):
        connection = sqlite3.connect(self.database)
        connection.executemany("INSERT INTO scan_observation_batches VALUES (?, ?)",
                               [(1, 0), (1, 1), (2, 0)])
        connection.commit()
        connection.close()
        self.assertEqual(rollout.observation_batches(self.database, [1]), 2)

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
    def stack(self, temp, tools, reuse, publication):
        args = rollout.stack_args(Path(temp) / "work", [Path(temp) / "input"], tools)
        stack = rollout.Stack.__new__(rollout.Stack)
        stack.reuse, stack.publication = reuse, publication
        stack.args, stack.roots, stack.node_id, stack.bin_dir = args, [], 3, tools
        return stack

    def test_per_file_publication_turns_on_the_agent_key_and_the_api_flag(self):
        with tempfile.TemporaryDirectory() as temp:
            tools = Path(temp) / "tools"
            tools.mkdir()
            env = {}
            for name in ("ffmpeg", "ffprobe", "mkvmerge"):
                (tools / name).touch()
                env[f"VOOM_{name.upper()}_BIN"] = str(tools / name)
            launched = []
            with patch.dict(os.environ, env), patch.object(
                    rollout.bootstrap.Bootstrap, "launch",
                    lambda _self, name, arguments: launched.append((name, arguments))):
                for publication, expected in (("completion", None), ("per-file", True)):
                    stack = self.stack(temp, tools, False, publication)
                    parsed = tomllib.loads(stack.configuration(Path(temp) / "token", "http://x"))
                    self.assertIs(parsed.get(rollout.PER_FILE_AGENT_KEY), expected)
                    stack.launch("voom-api", ["--bind", "x"])
                    stack.launch("voom-node-agent", ["--config", "y"])
        self.assertEqual(launched, [
            ("voom-api", ["--bind", "x"]), ("voom-node-agent", ["--config", "y"]),
            ("voom-api", ["--bind", "x", rollout.PER_FILE_API_FLAG]),
            ("voom-node-agent", ["--config", "y"])])

    def test_an_unknown_publication_is_refused_and_serve_keeps_completion(self):
        with self.assertRaises(rollout.HarnessError):
            rollout.Stack(argparse.Namespace(), False, "batch")
        with patch.object(rollout.bootstrap.Bootstrap, "__init__", lambda _self, _args: None):
            self.assertEqual(rollout.Stack(argparse.Namespace(), True).publication, "completion")

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
                    stack.publication = "completion"
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

    def test_each_proc_source_is_read_alone_and_an_unavailable_counter_names_its_cause(self):
        with tempfile.TemporaryDirectory() as scratch:
            proc = Path(scratch)
            (proc / "123").mkdir()
            for name, text in (("stat", "123 (w) S" + " 0" * 10 + " 1 1"),
                               ("status", "VmHWM:\t 4 kB\n"),
                               ("io", "rchar: 7\nread_bytes: 3\n")):
                (proc / "123" / name).write_text(text)
            stderr = io.StringIO()
            with patch.object(rollout.baseline, "PROC_ROOT", proc), redirect_stderr(stderr):
                usage = rollout.process_usage(123)
        self.assertEqual(usage, {"cpu_seconds": "unavailable", "peak_rss_bytes": 4096,
                                 "rchar": 7, "read_bytes": 3})
        self.assertEqual(stderr.getvalue().splitlines(), [
            "media-evidence-rollout: process cpu_seconds unavailable: stat: field absent"])

    @unittest.skipUnless(Path("/proc/self/io").exists(), "needs Linux /proc")
    def test_reads_this_process(self):
        usage = rollout.process_usage(os.getpid())
        self.assertTrue(all(isinstance(value, (int, float)) for value in usage.values()))

    @unittest.skipUnless(Path("/proc/self/stat").exists(), "needs Linux /proc")
    def test_worker_discovery_requires_one_hash_and_one_probe_worker(self):
        with self.assertRaises(rollout.HarnessError):
            rollout.worker_pids(os.getpid())


def cell(first_durable, evict=True, storage="local", ids=("f0001",), reuse=False, kill=None,
         publication="completion"):
    """A scan record reduced to what `compare_scans` reads."""
    repetitions = []
    for value in first_durable:
        workers = {name: {"cpu_seconds": 1.0, "rchar": 10, "read_bytes": 5}
                   for name in rollout.WORKER_PROGRAMS}
        repetitions.append({"first_durable_observation_seconds": value,
                            "complete_seconds": value, "workers": workers,
                            "host_counters": {"cifs_bytes_read": 7}})
    return {"corpus": {"label": "c", "selected_ids": list(ids), "storage_class": storage},
            "cell": {"evict": evict, "probe_hash_reuse": reuse, "kill": kill,
                     "publication": publication},
            "build": {"binaries_sha256": {"voom": "1"}}, "repetitions": repetitions}


class CompareTests(unittest.TestCase):
    def test_a_clearly_faster_cell_improves(self):
        result = rollout.compare_scans(cell([10.0, 10.1, 9.9]),
                                       cell([2.0, 2.1, 2.0], reuse=True))
        self.assertEqual(result["first_durable_observation"]["verdict"], "improvement")
        self.assertEqual(result["physical_bytes"]["baseline_median"], 10)

    def test_a_noisy_baseline_is_inconclusive(self):
        result = rollout.compare_scans(cell([10.0, 5.0, 6.0]), cell([1.0, 1.0, 1.0], reuse=True))
        self.assertEqual(result["first_durable_observation"]["verdict"], "inconclusive")

    def test_as_found_cells_drop_repetition_one_and_use_host_cifs_reads(self):
        # Repetition 1 of an as-found cell is reported separately by the protocol.
        base = cell([100.0, 10.0, 10.0], evict=False, storage="network")
        result = rollout.compare_scans(base, cell([1.0, 2.0, 2.0], evict=False,
                                                   storage="network", reuse=True))
        self.assertEqual(result["first_durable_observation"]["baseline_median"], 10.0)
        self.assertEqual(result["physical_bytes"]["baseline_median"], 7)

    def test_cells_that_differ_in_files_cache_or_build_do_not_compare(self):
        on = cell([1.0], reuse=True)
        for baseline in (cell([1.0], ids=("f0002",)), cell([1.0], evict=False)):
            with self.assertRaises(rollout.HarnessError):
                rollout.compare_scans(baseline, on)
        rebuilt = cell([1.0])
        rebuilt["build"]["binaries_sha256"] = {"voom": "2"}
        with self.assertRaises(rollout.HarnessError):
            rollout.compare_scans(rebuilt, on)

    def test_per_file_publication_compares_against_completion_with_reuse_fixed(self):
        for reuse in (False, True):
            result = rollout.compare_scans(
                cell([600.0, 601.0, 602.0], reuse=reuse),
                cell([20.0, 21.0, 20.5], reuse=reuse, publication="per-file"))
            self.assertEqual(result["first_durable_observation"]["verdict"], "improvement")

    def test_two_settings_or_a_reversed_publication_do_not_compare(self):
        for baseline, candidate in (
                (cell([1.0]), cell([1.0], reuse=True, publication="per-file")),
                (cell([1.0], publication="per-file"), cell([1.0])),
                (cell([1.0], publication="per-file"),
                 cell([1.0], reuse=True, publication="per-file", kill={"w": 1}))):
            with self.assertRaises(rollout.HarnessError):
                rollout.compare_scans(baseline, candidate)

    def test_a_schema_one_record_compares_as_completion(self):
        old = cell([10.0, 10.1, 9.9])
        del old["cell"]["publication"]
        result = rollout.compare_scans(old, cell([2.0, 2.1, 2.0], reuse=True))
        self.assertEqual(result["first_durable_observation"]["verdict"], "improvement")

    def test_only_an_off_baseline_against_an_on_candidate_compares(self):
        # A reversed pair or a kill cell would report a meaningless "improvement".
        for baseline, candidate in ((cell([1.0], reuse=True), cell([1.0])),
                                    (cell([1.0]), cell([1.0], reuse=True, kill={"w": 1}))):
            with self.assertRaises(rollout.HarnessError):
                rollout.compare_scans(baseline, candidate)


COMPARABLE_PAIRS = (
    ("rollout/2026-10-30-g3-stage0-off-local-corpus-evicted-after-816.json",
     "rollout/2026-10-30-g2-g3-on-local-corpus-evicted-after-816.json"),
    ("rollout/2026-10-30-canary-off-local-subset-evicted-after-816.json",
     "rollout/2026-10-30-canary-on-local-subset-evicted-after-816.json"),
    ("rollout/2026-10-30-net-off-network-subset-as-found.json",
     "rollout/2026-10-30-net-on-network-subset-as-found.json"),
    ("per-file-publication/2026-11-17-stage0-completion-local-corpus-evicted.json",
     "per-file-publication/2026-11-17-per-file-local-corpus-evicted.json"),
)


def never_compared(*_records):
    raise AssertionError("compare_scans ran on a record that failed validation")


class CompareInputTests(unittest.TestCase):
    """`compare` judges only records that pass the scan contract (#886)."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.baseline, self.candidate = (BASELINE / path for path in COMPARABLE_PAIRS[0])

    def written(self, name: str, text: str) -> Path:
        path = Path(self.temp.name) / name
        path.write_text(text, encoding="utf-8")
        return path

    def refusal(self, baseline: Path, candidate: Path) -> str:
        stderr = io.StringIO()
        with patch.object(rollout, "compare_scans", never_compared), redirect_stderr(stderr):
            self.assertEqual(rollout.main(["compare", str(baseline), str(candidate)]), 1)
        return stderr.getvalue()

    def test_compare_refuses_an_invalid_baseline(self):
        record = json.loads(self.baseline.read_text(encoding="utf-8"))
        del record["not_measured"]
        edited = self.written("edited.json", json.dumps(record))
        message = self.refusal(edited, self.candidate)
        self.assertIn(f"baseline {edited} failed validation: not_measured", message)
        self.assertNotIn("candidate", message)

    def test_compare_refuses_an_invalid_candidate(self):
        array = self.written("array.json", "[]")
        message = self.refusal(self.baseline, array)
        self.assertIn(f"candidate {array} failed validation: not a JSON object", message)
        self.assertNotIn("baseline", message)

    def test_compare_names_unparseable_and_malformed_records(self):
        truncated = self.written("truncated.json", self.baseline.read_text(encoding="utf-8")[:40])
        record = json.loads(self.candidate.read_text(encoding="utf-8"))
        record["cell"] = []
        malformed = self.written("malformed.json", json.dumps(record))
        message = self.refusal(truncated, malformed)
        self.assertIn(f"baseline {truncated} failed validation: not valid JSON", message)
        self.assertIn(f"candidate {malformed} failed validation: malformed sections", message)

    def test_committed_records_pass_compare_validation(self):
        paths = [path for directory in RECORD_DIRECTORIES for path in directory.glob("*.json")]
        for path in sorted(paths):
            record, problems = rollout.read_compared(path)
            if str(record.get("schema", "")).startswith("media-evidence-rollout-scan/"):
                with self.subTest(path.name):
                    self.assertEqual(problems, [])
        for pair in COMPARABLE_PAIRS:
            stdout = io.StringIO()
            with self.subTest(pair[0]), redirect_stdout(stdout):
                self.assertEqual(rollout.main(["compare", *(str(BASELINE / p) for p in pair)]), 0)
                self.assertIn("verdict", json.loads(stdout.getvalue())["complete"])


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

    def test_the_edit_starts_on_a_timer_of_the_given_delay(self):
        later = rollout.start_edit(5.0, lambda: None)
        later.cancel()
        self.assertEqual(later.interval, 5.0)
        ran = []
        rollout.start_edit(0.0, lambda: ran.append(True)).join()
        self.assertEqual(ran, [True])


class FakeWorker:
    """Hash and probe workers reduced to the stat checks the real ones make."""

    # Case (b)'s edit, handed over by the patched `start_edit` and run inside the hash read.
    edit = None

    def __init__(self, binary):
        self.program = Path(binary).name
        self.payloads = []

    @staticmethod
    def facts(path):
        stat = path.stat()
        return (stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns, stat.st_ino)

    def call(self, operation, payload):
        self.payloads.append(payload)
        if operation == "hash_file":
            path = Path(payload["provider_locator"], payload["provider_relative_locator"])
            before = self.facts(path)
            # The read takes time, so a coarse ctime still ticks past `before` (and, for
            # cases (a) and (c), past the hash) before any edit.
            time.sleep(0.3)
            edit, FakeWorker.edit = FakeWorker.edit, None
            if edit:
                edit()
            if self.facts(path) != before:
                return {"kind": "error", "code": rollout.MISMATCH,
                        "payload": {"stage": "hash_drift"}}
            FakeWorker.hashed = before
            return {"kind": "result", "payload": {
                "size_bytes": before[0], "content_hash": HASH_A, "modified_at": "t",
                "changed_at_ns": before[2], "file_key": {"dev": 1, "ino": before[3]}}}
        if "guard" not in payload or self.facts(Path(payload["path"])) != FakeWorker.hashed:
            return {"kind": "error", "code": rollout.MISMATCH,
                    "payload": {"stage": "pre_probe_guard"}}
        return {"kind": "result", "payload": {"hash_source": "reused"}}

    def stop(self):
        pass


class CaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.source = root / "source.mkv"
        self.source.write_bytes(bytes(range(256)) * 64)
        os.utime(self.source, ns=(1_000_000_000, 2_000_000_000))
        self.scratch = root / "scratch"
        self.scratch.mkdir()
        self.args = rollout.parse_args(["construct", str(self.source), "--label", "fixture",
                                        "--scratch", str(self.scratch), "--edit-delay", "0.25",
                                        "--output", str(root / "out.json")])
        for patcher in (patch.object(rollout.baseline, "Worker", FakeWorker),
                        patch.object(rollout, "start_edit", self.edit_in_hash)):
            patcher.start()
            self.addCleanup(patcher.stop)
        self.addCleanup(setattr, FakeWorker, "edit", None)
        self.delays = []

    def edit_in_hash(self, delay, edit):
        """Hand case (b)'s edit to the fake hash, so it lands inside the read on any runner."""
        self.delays.append(delay)
        FakeWorker.edit = edit
        return SimpleNamespace(join=lambda: None)

    def test_every_case_behaves_as_its_gate_defines(self):
        before = self.source.read_bytes()
        results = {name: rollout.run_case(name, self.args, "tmpfs") for name in rollout.CASES}
        self.assertEqual(results["unchanged"]["hash_source"], "reused")
        self.assertTrue(results["unchanged"]["guard_sent"])
        self.assertEqual(results["a_restored_mtime_edit_before_probe"]["marker"],
                         "pre_probe_guard")
        self.assertEqual(results["b_restored_mtime_edit_during_hash"]["marker"], "hash_drift")
        self.assertEqual(self.delays, [0.25])
        for name in list(rollout.CASES)[1:]:
            self.assertTrue(results[name]["detected"], name)
        # The source is never edited and every copy is removed.
        self.assertEqual(self.source.read_bytes(), before)
        self.assertEqual(self.source.stat().st_mtime_ns, 2_000_000_000)
        self.assertEqual(list(self.scratch.iterdir()), [])

    def test_restored_mtime_cases_keep_the_mtime_at_probe_time(self):
        seen = []
        original = FakeWorker.call

        def spy(worker, operation, payload):
            if operation == "probe_file":
                stat = Path(payload["path"]).stat()
                seen.append((stat.st_mtime_ns, stat.st_ctime_ns))
            return original(worker, operation, payload)

        with patch.object(FakeWorker, "call", spy):
            result = rollout.run_case("a_restored_mtime_edit_before_probe", self.args, "tmpfs")
        # Same mtime as the hash saw, so only the ctime can reveal the edit.
        self.assertEqual(seen[0][0], FakeWorker.hashed[1])
        self.assertNotEqual(seen[0][1], FakeWorker.hashed[2])
        self.assertTrue(result["detected"])

    def test_only_a_checksum_mismatch_counts_as_detected(self):
        failing = {"kind": "error", "code": "EXTERNAL_SYSTEM_UNAVAILABLE", "payload": {}}
        with patch.object(FakeWorker, "call", lambda *_: failing):
            result = rollout.run_case("c_mtime_moving_write_before_probe", self.args, "tmpfs")
        self.assertFalse(result["detected"])

    def test_an_edit_after_the_hash_read_refuses_case_b(self):
        joined = []

        def after_hash(_, edit):
            # The edit runs only when the case joins it, which is after the hash read returned.
            def join():
                edit()
                joined.append(True)
            return SimpleNamespace(join=join)

        with patch.object(rollout, "start_edit", after_hash):
            with self.assertRaises(rollout.HarnessError):
                rollout.run_case("b_restored_mtime_edit_during_hash", self.args, "tmpfs")
        # The case waits for its edit before refusing, so no edit outlives the case's copy.
        self.assertEqual(len(joined), 1)

    def test_a_hash_result_without_guard_facts_refuses_the_probe(self):
        def no_guard_facts(worker, operation, payload):
            return {"kind": "result", "payload": {"size_bytes": 1, "content_hash": HASH_A,
                                                  "modified_at": "t"}}
        with patch.object(FakeWorker, "call", no_guard_facts):
            with self.assertRaises(rollout.HarnessError):
                rollout.run_case("unchanged", self.args, "tmpfs")

    def test_a_copy_that_is_the_source_is_refused(self):
        # A hard link passes every file-type check; only the inode comparison refuses it.
        with patch.object(rollout, "copy_bytes", os.link):
            with self.assertRaises(rollout.HarnessError):
                rollout.copy_source(self.source, self.scratch / "copy.mkv")

    @unittest.skipUnless(Path("/proc/self/stat").exists(), "needs Linux /proc")
    def test_a_kill_spares_a_pid_that_is_no_longer_the_worker(self):
        with patch.object(rollout, "start_time", side_effect=["1", "2"]), \
                patch.object(rollout.os, "kill") as kill:
            rollout.kill_at(os.getpid(), time.monotonic()).join()
        kill.assert_not_called()

    def test_a_label_naming_the_host_or_user_is_refused(self):
        for label in ("Upper", "/abs", rollout.getpass.getuser().lower() + "-media"):
            with self.assertRaises(rollout.HarnessError):
                rollout.public_label(label)
        self.assertEqual(rollout.public_label("local-corpus-f0004"), "local-corpus-f0004")


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
            "batches": 1, "published_before_complete": 0,
        }
        record = {"schema": rollout.SCAN_SCHEMA, "repetitions": [repetition],
                  "not_measured": dict(rollout.NOT_MEASURED)}
        for section, keys in rollout.SCAN_KEYS.items():
            record[section] = dict.fromkeys(keys, "x")
        record["cell"]["kill"] = None
        record["cell"]["publication"] = "completion"
        record["build"]["binaries_git_changed_paths"] = []
        return record

    def per_file_record(self):
        record = self.scan_record()
        record["cell"]["publication"] = "per-file"
        record["corpus"]["roots"] = 1
        repetition = record["repetitions"][0]
        repetition["evidence"]["observations"] = 34
        repetition["batches"], repetition["published_before_complete"] = 34, 33
        return record

    def test_a_per_file_record_proves_both_halves_of_the_prototype(self):
        self.assertEqual(rollout.validate_scan(self.per_file_record()), [])
        record = self.per_file_record()
        record["repetitions"][0]["batches"] = 1
        self.assertIn("repetition 1 per-file batches", rollout.validate_scan(record))
        record = self.per_file_record()
        record["repetitions"][0]["published_before_complete"] = 0
        self.assertIn("repetition 1 per-file publication", rollout.validate_scan(record))

    def test_a_multi_root_per_file_cell_is_refused(self):
        # Cell-wide aggregates: the other root's in-session publication would cover a root
        # that published only at completion, so the proof holds only for one root.
        record = self.per_file_record()
        record["corpus"]["roots"] = 2
        self.assertIn("per-file roots", rollout.validate_scan(record))
        record["corpus"]["roots"] = "x"
        self.assertIn("per-file roots", rollout.validate_scan(record))
        record["corpus"]["roots"] = 2
        record["cell"]["publication"] = "completion"
        self.assertEqual(rollout.validate_scan(record), [])

    def test_a_per_file_capture_refuses_more_than_one_root(self):
        with tempfile.TemporaryDirectory() as temp:
            args = rollout.parse_args([
                "scan", str(Path(temp) / "manifest.json"), "--reuse", "off",
                "--publication", "per-file", "--workdir", str(Path(temp) / "work"),
                "--output", str(Path(temp) / "out"), "--repetitions", "1"])
            # A two-file subset in separate directories: scan_roots gives each its own root.
            files = [{"id": name, "relative": f"{name}/movie.mkv"} for name in ("a", "b", "c")]
            Path(args.manifest).write_text(json.dumps({"label": "local-corpus", "root": temp,
                                                       "files": files}))
            with patch.object(rollout, "binaries_build", lambda _bin: {}), \
                    patch.object(rollout.baseline, "select_files",
                                 lambda manifest, _ids: manifest["files"][:2]), \
                    patch.object(rollout.baseline, "corpus_filesystem", lambda *_: "btrfs"), \
                    patch.object(rollout, "scan_repetition",
                                 lambda *_: self.fail("a refused capture ran a repetition")):
                with self.assertRaisesRegex(rollout.HarnessError, "2 roots"):
                    rollout.capture_scan(args)
            self.assertFalse(args.workdir.exists())

    def test_a_failed_session_voids_a_clean_cell(self):
        record = self.per_file_record()
        record["repetitions"][0]["session_status"] = ["failed"]
        self.assertIn("repetition 1 sessions", rollout.validate_scan(record))

    def test_schema_two_needs_its_keys_and_a_clean_binaries_checkout(self):
        record = self.scan_record()
        del record["cell"]["publication"]
        del record["repetitions"][0]["batches"]
        problems = rollout.validate_scan(record)
        self.assertEqual(problems, ["cell publication", "repetition batches"])
        record = self.scan_record()
        record["build"]["binaries_git_changed_paths"] = ["crates/x.rs"]
        self.assertIn("binaries checkout dirty", rollout.validate_scan(record))
        record = self.scan_record()
        record["cell"]["publication"] = "batch"
        self.assertIn("publication", rollout.validate_scan(record))

    def test_schema_one_records_keep_their_own_rules(self):
        record = self.scan_record()
        record["schema"] = rollout.SCAN_SCHEMA_V1
        del record["cell"]["publication"]
        for key in ("binaries_git_commit", "binaries_git_changed_paths"):
            del record["build"][key]
        for key in ("batches", "published_before_complete"):
            del record["repetitions"][0][key]
        self.assertEqual(rollout.validate_scan(record), [])

    def test_a_capture_refuses_binaries_that_change_underneath_it(self):
        builds = iter([{"binaries_git_commit": "a"}, {"binaries_git_commit": "b"}])
        with tempfile.TemporaryDirectory() as temp:
            args = rollout.parse_args([
                "scan", str(Path(temp) / "manifest.json"), "--reuse", "off",
                "--workdir", str(Path(temp) / "work"), "--output", str(Path(temp) / "out"),
                "--repetitions", "1"])
            Path(args.manifest).write_text(json.dumps({"label": "local-corpus", "root": temp,
                                                       "files": []}))
            with patch.object(rollout, "binaries_build", lambda _bin: next(builds)), \
                    patch.object(rollout.baseline, "select_files", lambda *_: []), \
                    patch.object(rollout.baseline, "corpus_filesystem", lambda *_: "btrfs"), \
                    patch.object(rollout, "scan_repetition", lambda *_: {}):
                with self.assertRaises(rollout.HarnessError):
                    rollout.capture_scan(args)

    def test_a_complete_scan_record_validates(self):
        self.assertEqual(rollout.validate_scan(self.scan_record()), [])

    def test_a_host_path_is_refused(self):
        record = self.scan_record()
        record["corpus"]["label"] = "share/Users/someone/media"
        self.assertIn("host path", rollout.validate_scan(record))

    def test_any_absolute_path_is_refused(self):
        record = self.scan_record()
        record["corpus"]["filesystem_type"] = "/srv/media"
        self.assertIn("host path", rollout.validate_scan(record))

    def test_a_kill_cell_whose_worker_never_restarted_is_refused(self):
        record = self.scan_record()
        record["cell"]["kill"] = {"worker": "hash", "after_seconds": 3}
        self.assertIn("kill not delivered", rollout.validate_scan(record))
        record["repetitions"][0]["worker_restarted"] = True
        self.assertEqual(rollout.validate_scan(record), [])

    def test_not_measured_must_be_complete(self):
        record = self.scan_record()
        del record["not_measured"]["retries"]
        self.assertIn("not_measured", rollout.validate_scan(record))

    def test_a_missing_metric_is_refused(self):
        record = self.scan_record()
        del record["repetitions"][0]["first_durable_observation_seconds"]
        self.assertNotEqual(rollout.validate_scan(record), [])
        record = self.scan_record()
        record["repetitions"][0]["workers"]["probe"]["rchar"] = None
        self.assertIn("worker probe metric", rollout.validate_scan(record))

    def test_a_construct_record_needs_every_case_in_order(self):
        cases = [{"case": name, "stage": "probe", "result": "failure", "detected": True,
                  "guard_sent": True} for name in rollout.CASES]
        record = {"schema": rollout.CONSTRUCT_SCHEMA, "cases": cases}
        self.assertEqual(rollout.validate_construct(record), [])
        record["cases"] = cases[1:]
        self.assertIn("cases", rollout.validate_construct(record))
        record["cases"] = [dict(case, guard_sent=False) for case in cases]
        self.assertIn("case unchanged guard", rollout.validate_construct(record))

    def test_committed_records_validate(self):
        # G1's harness cells keep the #788 schema and its validator.
        validators = {rollout.SCAN_SCHEMA: rollout.validate_scan,
                      rollout.SCAN_SCHEMA_V1: rollout.validate_scan,
                      rollout.CONSTRUCT_SCHEMA: rollout.validate_construct,
                      rollout.baseline.SCHEMA: rollout.baseline.validate_run}
        paths = [path for directory in RECORD_DIRECTORIES for path in directory.glob("*.json")]
        for path in sorted(paths):
            record = json.loads(path.read_text(encoding="utf-8"))
            with self.subTest(path.name):
                self.assertIn(record.get("schema"), validators)
                self.assertEqual(validators[record["schema"]](record), [])


if __name__ == "__main__":
    unittest.main()
