#!/usr/bin/env python3
"""Prove the #898 copy-loop harness copies, cleans up, redacts and judges as its spec states.

The stub example speaks the `copy-commit` line protocol: it copies each file into the scratch
directory it is given, removes the copy, and reports a SHA-256 digest in place of BLAKE3, so
the harness runs without a Rust build; the real example calls production
`stream_copy_with_hash`. Comparison fixtures are synthetic records.
"""

import contextlib
import hashlib
import importlib.util
import io
import json
import os
import stat
import sys
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path
from unittest import mock


def load_script(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


copy = load_script("media_copy", "media-evidence-copy.py")
RECORD_DIR = copy.REPO_ROOT / "docs" / "media-evidence-baseline" / "copy"
MIB = 1024 * 1024

STUB = """\
import hashlib, os, shutil, sys, time
scratch = sys.argv[2]
paths = []
for line in sys.stdin:
    if line == "\\n":
        break
    paths.append(line.rstrip("\\n"))
start = time.monotonic()
for index, path in enumerate(paths):
    begin = time.monotonic() - start
    temp = os.path.join(scratch, f"copy-{index}.tmp")
    try:
        shutil.copyfile(path, temp)
    except OSError:
        print(f"err {index} {begin:.6f} 0.000100 artifact_unavailable", flush=True)
        continue
    data = open(temp, "rb").read()
    if not os.environ.get("STUB_LEAVE_COPY"):
        os.remove(temp)
    digest = hashlib.sha256(data).hexdigest()
    print(f"ok {index} {begin:.6f} 0.000100 {len(data)} blake3:{digest}", flush=True)
print("done", flush=True)
sys.stdin.read()
"""


class Corpus:
    """A scratch corpus beside an empty scratch directory, a private manifest and a stub."""

    def __init__(self, test, contents):
        scratch = tempfile.TemporaryDirectory()
        test.addCleanup(scratch.cleanup)
        self.dir = Path(scratch.name)
        self.root = self.dir / "media"
        self.root.mkdir()
        self.scratch = self.dir / "copies"
        self.scratch.mkdir()
        files = []
        for index, (name, data) in enumerate(contents, start=1):
            (self.root / name).write_bytes(data)
            files.append({"id": f"f{index:04d}", "relative": name, "size_bytes": len(data),
                          "mtime_ns": 0, "ext": ".mkv"})
        self.manifest = self.dir / "manifest.json"
        self.manifest.write_text(json.dumps({"label": "local-corpus", "root": str(self.root),
                                             "files": files}))
        self.examples = self.dir / "examples"
        self.examples.mkdir()
        stub = self.examples / "copy-commit"
        stub.write_text(f"#!{sys.executable}\n{STUB}")
        stub.chmod(stub.stat().st_mode | stat.S_IXUSR)

    def capture(self, **overrides):
        """Capture as the harness of a checkout that holds this corpus's stub example."""
        args = Namespace(manifest=self.manifest, scratch_dir=self.scratch,
                         example_dir=self.examples, ids=None, max_bytes=10**12, repetitions=2,
                         evict=False)
        for key, value in overrides.items():
            setattr(args, key, value)
        with mock.patch.object(copy, "REPO_ROOT", self.dir.resolve()):
            return copy.capture(args)


class RunTests(unittest.TestCase):
    def test_one_copy_per_file_with_the_hash_set_no_host_path_and_no_copy_left(self):
        corpus = Corpus(self, [("a.mkv", b"abc"), ("b.mkv", b"x" * 5000)])
        record = corpus.capture()
        self.assertEqual(copy.validate_run(record), [])
        first, second = record["repetitions"]
        self.assertEqual([row["outcome"] for row in first["files"]], ["copied", "copied"])
        self.assertEqual(first["logical_bytes_read"], 5003)
        expected = hashlib.sha256("\n".join(sorted(
            f"{file_id} blake3:{hashlib.sha256(data).hexdigest()}"
            for file_id, data in (("f0001", b"abc"), ("f0002", b"x" * 5000)))).encode())
        self.assertEqual(first["hash_set_sha256"], expected.hexdigest())
        self.assertEqual(first["hash_set_sha256"], second["hash_set_sha256"])
        if Path("/proc/self/io").exists():
            self.assertGreaterEqual(first["process"]["wchar"], 5003)
        self.assertNotIn(str(corpus.dir), json.dumps(record))
        self.assertEqual(record["parameters"]["copy_buffer_bytes"], 16 * 1024)
        self.assertEqual(record["parameters"]["concurrency"], 1)
        self.assertEqual(list(corpus.scratch.iterdir()), [])

    def test_a_failed_copy_is_a_row_with_its_name_and_no_bytes(self):
        corpus = Corpus(self, [("a.mkv", b"abc"), ("gone.mkv", b"zz")])
        (corpus.root / "gone.mkv").unlink()
        repetition = corpus.capture(repetitions=1)["repetitions"][0]
        failed = repetition["files"][1]
        self.assertEqual((failed["outcome"], failed["failure"]),
                         ("copy_failed", "artifact_unavailable"))
        self.assertEqual(repetition["logical_bytes_read"], 3)

    def test_a_copy_left_in_the_scratch_directory_stops_the_run(self):
        corpus = Corpus(self, [("a.mkv", b"abc")])
        with mock.patch.dict(os.environ, {"STUB_LEAVE_COPY": "1"}), \
                self.assertRaisesRegex(copy.HarnessError, "not empty"):
            corpus.capture(repetitions=1)


    def test_an_example_built_outside_this_checkout_is_refused(self):
        corpus = Corpus(self, [("a.mkv", b"abc")])
        args = Namespace(manifest=corpus.manifest, scratch_dir=corpus.scratch,
                         example_dir=corpus.examples, ids=None, max_bytes=10**12,
                         repetitions=1, evict=False)
        with self.assertRaisesRegex(copy.HarnessError, "outside this checkout"):
            copy.capture(args)

    def test_a_failed_run_that_leaves_a_partial_copy_says_so(self):
        corpus = Corpus(self, [("a.mkv", b"abc")])
        stub = corpus.examples / "copy-commit"
        stub.write_text(f"#!{sys.executable}\nimport sys\n"
                        "open(sys.argv[2] + '/copy-0.tmp', 'wb').write(b'ab')\n")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), \
                self.assertRaisesRegex(copy.HarnessError, "exited before reporting done"):
            corpus.capture(repetitions=1)
        self.assertIn("still holds a partial copy", stderr.getvalue())


class ScratchTests(unittest.TestCase):
    def test_missing_non_empty_and_inside_corpus_scratch_directories_are_refused(self):
        corpus = Corpus(self, [("a.mkv", b"abc")])
        inside = corpus.root / "copies"
        inside.mkdir()
        (corpus.scratch / "left.tmp").write_bytes(b"")
        for scratch, message in ((corpus.dir / "absent", "existing directory"),
                                 (corpus.scratch, "not empty"),
                                 (corpus.root, "inside the corpus root"),
                                 (inside, "inside the corpus root")):
            with self.subTest(scratch=scratch.name), \
                    self.assertRaisesRegex(copy.HarnessError, message):
                copy.check_scratch(scratch, corpus.root, 3)

    def test_a_scratch_directory_on_another_filesystem_is_refused(self):
        corpus = Corpus(self, [("a.mkv", b"abc")])
        real_stat = Path.stat

        def other_device(path, *args, **kwargs):
            result = real_stat(path, *args, **kwargs)
            if path == corpus.scratch.resolve():
                values = list(result)
                values[stat.ST_DEV] += 1
                return os.stat_result(values)
            return result

        with mock.patch.object(Path, "stat", other_device), \
                self.assertRaisesRegex(copy.HarnessError, "filesystem"):
            copy.check_scratch(corpus.scratch, corpus.root, 3)
        copy.check_scratch(corpus.scratch, corpus.root, 3)

    def test_a_filesystem_without_room_for_two_of_the_largest_file_is_refused(self):
        corpus = Corpus(self, [("a.mkv", b"abc")])
        forty_bytes_free = Namespace(f_bavail=10, f_frsize=4)
        with mock.patch.object(copy.os, "statvfs", return_value=forty_bytes_free):
            copy.check_scratch(corpus.scratch, corpus.root, 20)
            with self.assertRaisesRegex(copy.HarnessError, "free space"):
                copy.check_scratch(corpus.scratch, corpus.root, 21)


class ProcessUsageTests(unittest.TestCase):
    STAT = "123 (copy commit) S" + " 0" * 10 + " 100 50 25 25 0 0"
    STATUS = "Name:\tcopy-commit\nVmHWM:\t    2048 kB\n"
    IO = "rchar: 5000\nwchar: 6000\nread_bytes: 4096\nwrite_bytes: 8192\n"

    def usage(self, io_text):
        with tempfile.TemporaryDirectory() as scratch:
            proc = Path(scratch) / "123"
            proc.mkdir()
            for name, text in (("stat", self.STAT), ("status", self.STATUS), ("io", io_text)):
                (proc / name).write_text(text)
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                usage = copy.process_usage(123, Path(scratch))
        return usage, stderr.getvalue().splitlines()

    def test_write_counters_join_observe_counters(self):
        usage, lines = self.usage(self.IO)
        self.assertEqual(usage, {"cpu_seconds": round(200 / os.sysconf("SC_CLK_TCK"), 3),
                                 "peak_rss_bytes": 2048 * 1024, "rchar": 5000, "wchar": 6000,
                                 "read_bytes": 4096, "write_bytes": 8192})
        self.assertEqual(lines, [])

    def test_a_missing_write_counter_is_unavailable_with_its_cause(self):
        usage, lines = self.usage("rchar: 5000\nwchar: 6000\nread_bytes: 4096\n")
        self.assertEqual(usage["write_bytes"], copy.UNAVAILABLE)
        self.assertEqual(lines, ["media-evidence-copy: process write_bytes unavailable: "
                                 "io: field absent"])


class BufferTests(unittest.TestCase):
    def test_the_copy_loop_reads_16k_today_and_the_pattern_reads_the_1m_variant(self):
        self.assertEqual(copy.buffer_bytes(), 16 * 1024)
        with tempfile.TemporaryDirectory() as scratch:
            source = Path(scratch) / copy.COPY_SOURCE
            source.parent.mkdir(parents=True)
            source.write_text(copy.candidates.COPY_1M + "\n")
            self.assertEqual(copy.buffer_bytes(Path(scratch)), MIB)


def valid_record():
    row = {"id": "f0001", "size_bytes": 3, "ext": ".mkv", "resident_bytes_before": 0,
           "queue_delay_seconds": 0.0, "copy_seconds": 1.0, "outcome": "copied",
           "failure": None}
    return {
        "schema": copy.SCHEMA,
        "corpus": {"label": "local-corpus", "storage_class": "local", "filesystem_type": "btrfs",
                   "manifest_files": 1, "selected_ids": ["f0001"], "skipped_budget_ids": []},
        "build": {"git_commit": "a" * 40, "git_changed_paths": [], "harness_sha256": "b" * 64,
                  "example_sha256": "c" * 64},
        "host": {"os": "Linux", "machine": "x86_64", "cpu_count": 1, "memory_bytes": 1,
                 "python": "3"},
        "parameters": {"path": copy.PATH, "copy_buffer_bytes": 16384, "concurrency": 1,
                       "max_bytes_per_repetition": 10, "repetitions": 1, "evict": True},
        "repetitions": [{
            "index": 1, "evicted": True, "wall_seconds": 1.0, "logical_bytes_read": 3,
            "hash_set_sha256": "d" * 64,
            "process": {"cpu_seconds": 1.0, "peak_rss_bytes": 1, "rchar": 3, "read_bytes": 3,
                        "wchar": 3, "write_bytes": 3},
            "host_counters": {"network_rx_bytes": 0, "cifs_bytes_read": "unavailable"},
            "files": [row],
        }],
        "not_measured": dict(copy.NOT_MEASURED),
    }


class ValidateTests(unittest.TestCase):
    def test_a_complete_record_is_valid(self):
        self.assertEqual(copy.validate_run(valid_record()), [])

    def test_each_contract_violation_is_reported(self):
        cases = {
            "schema": lambda r: r.update(schema="media-evidence-observe-run/1"),
            "parameters path": lambda r: r["parameters"].update(path="ffmpeg"),
            "parameters copy_buffer_bytes": lambda r: r["parameters"].pop("copy_buffer_bytes"),
            "process write_bytes": lambda r: r["repetitions"][0]["process"].pop("write_bytes"),
            "process wchar": lambda r: r["repetitions"][0]["process"].update(wchar=-1),
            "file copy_seconds": lambda r: r["repetitions"][0]["files"][0].pop("copy_seconds"),
            "file outcome": lambda r: r["repetitions"][0]["files"][0].update(outcome="observed"),
            "file failure": lambda r: r["repetitions"][0]["files"][0].update(failure="x"),
            "host path": lambda r: r["corpus"].update(label="/home/someone/media"),
            "per-file hash":
                lambda r: r["repetitions"][0]["files"][0].update(ext="blake3:" + "0" * 64),
            "not_measured": lambda r: r["not_measured"].pop("removal"),
        }
        for problem, mutate in cases.items():
            with self.subTest(problem=problem):
                record = valid_record()
                mutate(record)
                self.assertIn(problem, copy.validate_run(record))

    def test_a_failed_row_needs_a_name(self):
        record = valid_record()
        record["repetitions"][0]["files"][0].update(outcome="copy_failed", failure=None)
        self.assertIn("file failure", copy.validate_run(record))
        record["repetitions"][0]["files"][0]["failure"] = "remove_failed"
        self.assertEqual(copy.validate_run(record), [])

    def test_committed_records_are_valid(self):
        for path in sorted(RECORD_DIR.glob("*.json")):
            with self.subTest(record=path.name):
                self.assertEqual(copy.validate_run(json.loads(path.read_text())), [])


def cell(buffer, walls, cpu, rss=2 * MIB, evict=True, hash_set="d" * 64, outcome="copied",
         ids=("f0001",)):
    record = valid_record()
    record["corpus"]["selected_ids"] = list(ids)
    record["parameters"].update(copy_buffer_bytes=buffer, evict=evict)
    repetitions = []
    for index, (wall, seconds) in enumerate(zip(walls, cpu), start=1):
        repetition = json.loads(json.dumps(record["repetitions"][0]))
        repetition.update(index=index, wall_seconds=wall, hash_set_sha256=hash_set)
        repetition["process"].update(cpu_seconds=seconds, peak_rss_bytes=rss)
        repetition["files"][0].update(copy_seconds=wall, outcome=outcome)
        repetitions.append(repetition)
    record["repetitions"] = repetitions
    return record


BASE = dict(buffer=16384, walls=[100.0, 100.0, 100.0], cpu=[50.0, 51.0, 49.0])


class CompareTests(unittest.TestCase):
    def test_cpu_improvement_without_regression_is_go(self):
        result = copy.compare(cell(**BASE), cell(MIB, [99.0, 100.0, 100.0], [20.0] * 3))
        self.assertEqual(result["metrics"]["cpu"]["verdict"], "improvement")
        self.assertIn("physical_write_bytes", result["metrics"])
        self.assertEqual(result["verdict"], "go")

    def test_unchanged_cpu_is_inconclusive(self):
        result = copy.compare(cell(**BASE), cell(MIB, [100.0] * 3, [50.0] * 3))
        self.assertEqual(result["verdict"], "inconclusive")

    def test_a_wall_regression_is_no_go_despite_cpu(self):
        result = copy.compare(cell(**BASE), cell(MIB, [120.0] * 3, [20.0] * 3))
        self.assertEqual(result["metrics"]["wall"]["verdict"], "regression")
        self.assertEqual(result["verdict"], "no-go")

    def test_a_copy_seconds_regression_alone_is_no_go(self):
        candidate = cell(MIB, [100.0] * 3, [20.0] * 3)
        for repetition in candidate["repetitions"]:
            repetition["files"][0]["copy_seconds"] = 130.0
        result = copy.compare(cell(**BASE), candidate)
        self.assertEqual(result["metrics"]["wall"]["verdict"], "inconclusive")
        self.assertEqual(result["metrics"]["copy_seconds"]["verdict"], "regression")
        self.assertEqual(result["verdict"], "no-go")

    def test_a_failed_row_differing_hashes_or_a_guardrail_is_no_go(self):
        for candidate in (cell(MIB, [99.0] * 3, [20.0] * 3, outcome="copy_failed"),
                          cell(MIB, [99.0] * 3, [20.0] * 3, hash_set="e" * 64),
                          cell(MIB, [99.0] * 3, [20.0] * 3, rss=300 * MIB)):
            with self.subTest():
                self.assertEqual(copy.compare(cell(**BASE), candidate)["verdict"], "no-go")

    def test_as_found_compares_repetitions_after_the_first(self):
        base = cell(16384, [300.0, 100.0, 100.0], [50.0, 50.0, 51.0], evict=False)
        cand = cell(MIB, [10.0, 100.0, 100.0], [20.0, 20.0, 20.0], evict=False)
        wall = copy.compare(base, cand)["metrics"]["wall"]
        self.assertEqual((wall["baseline_first"], wall["candidate_first"]), (300.0, 10.0))
        self.assertEqual(wall["baseline_median"], 100.0)

    def test_mismatched_cells_are_refused(self):
        for candidate in (cell(MIB, [99.0] * 3, [20.0] * 3, evict=False),
                          cell(MIB, [99.0] * 3, [20.0] * 3, ids=("f0002",)),
                          cell(16384, [99.0] * 3, [20.0] * 3)):
            with self.subTest(), self.assertRaises(copy.HarnessError):
                copy.compare(cell(**BASE), candidate)

    def test_render_names_the_loop_cache_state_and_verdict(self):
        base, cand = cell(**BASE), cell(MIB, [99.0, 100.0, 100.0], [20.0] * 3)
        text = "\n".join(copy.render(base, cand, copy.compare(base, cand)))
        self.assertIn("### node-agent-copy, evicted: 16384 vs 1048576 bytes", text)
        self.assertIn("Verdict: **go**.", text)


if __name__ == "__main__":
    unittest.main()
