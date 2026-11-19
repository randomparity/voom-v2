#!/usr/bin/env python3
"""Prove the #838 observe-path harness accounts, redacts and judges as its spec states.

The stub example speaks the `observe-<path>` line protocol with a SHA-256 digest in place of
BLAKE3, so the harness's accounting runs without a Rust build; the real examples call
production `observe_file_facts`. Comparison fixtures are synthetic records.
"""

import contextlib
import hashlib
import io
import os
import importlib.util
import json
import stat
import sys
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path


def load_script(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


observe = load_script("media_observe", "media-evidence-observe.py")
RECORD_DIR = observe.REPO_ROOT / "docs" / "media-evidence-baseline" / "observe"
MIB = 1024 * 1024

STUB = """\
import hashlib, sys, time
concurrency = int(sys.argv[1])
paths = []
for line in sys.stdin:
    if line == "\\n":
        break
    paths.append(line.rstrip("\\n"))
start = time.monotonic()
for index, path in enumerate(paths):
    begin = time.monotonic() - start
    try:
        data = open(path, "rb").read()
    except OSError:
        print(f"err {index} {begin:.6f} 0.000100 artifact_unavailable", flush=True)
        continue
    digest = hashlib.sha256(data).hexdigest()
    print(f"ok {index} {begin:.6f} 0.000100 {len(data)} blake3:{digest}", flush=True)
print("done", flush=True)
sys.stdin.read()
"""


class Corpus:
    """A scratch corpus, its private manifest and a stub example, removed on cleanup."""

    def __init__(self, test, contents):
        scratch = tempfile.TemporaryDirectory()
        test.addCleanup(scratch.cleanup)
        self.dir = Path(scratch.name)
        root = self.dir / "media"
        root.mkdir()
        files = []
        for index, (name, data) in enumerate(contents, start=1):
            (root / name).write_bytes(data)
            files.append({"id": f"f{index:04d}", "relative": name, "size_bytes": len(data),
                          "mtime_ns": 0, "ext": ".mkv"})
        self.manifest = self.dir / "manifest.json"
        self.manifest.write_text(json.dumps({"label": "local-corpus", "root": str(root),
                                             "files": files}))
        self.examples = self.dir / "examples"
        self.examples.mkdir()
        stub = self.examples / "observe-ffmpeg"
        stub.write_text(f"#!{sys.executable}\n{STUB}")
        stub.chmod(stub.stat().st_mode | stat.S_IXUSR)

    def capture(self, **overrides):
        args = Namespace(manifest=self.manifest, path="ffmpeg", example_dir=self.examples,
                         ids=None, max_bytes=10**12, repetitions=2, concurrency=1, evict=False)
        for key, value in overrides.items():
            setattr(args, key, value)
        return observe.capture(args)


class RunTests(unittest.TestCase):
    def test_one_read_per_file_with_the_hash_set_and_no_host_path(self):
        corpus = Corpus(self, [("a.mkv", b"abc"), ("b.mkv", b"x" * 5000)])
        record = corpus.capture()
        self.assertEqual(observe.validate_run(record), [])
        first, second = record["repetitions"]
        self.assertEqual([row["outcome"] for row in first["files"]], ["observed", "observed"])
        self.assertEqual(first["logical_bytes_read"], 5003)
        expected = hashlib.sha256("\n".join(sorted(
            f"{file_id} blake3:{hashlib.sha256(data).hexdigest()}"
            for file_id, data in (("f0001", b"abc"), ("f0002", b"x" * 5000)))).encode())
        self.assertEqual(first["hash_set_sha256"], expected.hexdigest())
        self.assertEqual(first["hash_set_sha256"], second["hash_set_sha256"])
        # The protocol records a counter the host lacks as `unavailable` (no /proc on BSD).
        if Path("/proc/self/stat").exists():
            self.assertIsInstance(first["process"]["cpu_seconds"], float)
        else:
            self.assertEqual(first["process"]["cpu_seconds"], observe.UNAVAILABLE)
        self.assertNotIn(str(corpus.dir), json.dumps(record))
        self.assertEqual(record["parameters"]["observe_buffer_bytes"], MIB)

    def test_a_failed_read_is_a_row_with_its_variant_and_no_bytes(self):
        corpus = Corpus(self, [("a.mkv", b"abc"), ("gone.mkv", b"zz")])
        (corpus.dir / "media" / "gone.mkv").unlink()
        repetition = corpus.capture(repetitions=1)["repetitions"][0]
        failed = repetition["files"][1]
        self.assertEqual((failed["outcome"], failed["failure"]),
                         ("observe_failed", "artifact_unavailable"))
        self.assertEqual(repetition["logical_bytes_read"], 3)

    def test_a_size_other_than_the_manifest_fails_the_row(self):
        corpus = Corpus(self, [("a.mkv", b"abc")])
        (corpus.dir / "media" / "a.mkv").write_bytes(b"abcd")
        row = corpus.capture(repetitions=1)["repetitions"][0]["files"][0]
        self.assertEqual((row["outcome"], row["failure"]), ("observe_failed", "size_changed"))

    def test_files_past_the_budget_are_skipped_and_recorded(self):
        corpus = Corpus(self, [("a.mkv", b"abc"), ("b.mkv", b"x" * 100)])
        record = corpus.capture(max_bytes=50, repetitions=1)
        self.assertEqual(record["corpus"]["selected_ids"], ["f0001"])
        self.assertEqual(record["corpus"]["skipped_budget_ids"], ["f0002"])

    def test_malformed_or_incomplete_example_output_is_refused(self):
        files = [{"id": "f0001", "size_bytes": 3, "ext": ".mkv"},
                 {"id": "f0002", "size_bytes": 3, "ext": ".mkv"}]
        for lines in (["ok 0 0 0 3"], ["ok 0 0 0 3 h", "ok 0 0 0 3 h"], ["ok 0 0 0 3 h"],
                      ["ok 7 0 0 3 h", "ok 1 0 0 3 h"], ["boom"]):
            with self.subTest(lines=lines), self.assertRaises(observe.HarnessError):
                observe.parse_rows(lines, files)


class ProcessUsageTests(unittest.TestCase):
    """`unavailable` stays a valid value; stderr names each unavailable counter's cause."""

    STAT = "123 (observe ffmpeg) S" + " 0" * 10 + " 100 50 25 25 0 0"
    STATUS = "Name:\tobserve-ffmpeg\nVmHWM:\t    2048 kB\n"
    IO = "rchar: 5000\nwchar: 0\nread_bytes: 4096\n"

    def usage(self, sources=None, make_root=True):
        """`process_usage` over a scratch /proc holding `sources` for pid 123."""
        with tempfile.TemporaryDirectory() as scratch:
            proc = Path(scratch) / "proc"
            if make_root:
                proc.mkdir()
            if sources is not None:
                (proc / "123").mkdir()
                for name, text in sources.items():
                    (proc / "123" / name).write_text(text)
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                usage = observe.process_usage(123, proc)
        return usage, stderr.getvalue().splitlines()

    def complete(self, **overrides):
        return {"stat": self.STAT, "status": self.STATUS, "io": self.IO, **overrides}

    def assert_causes(self, lines, expected):
        self.assertEqual(lines, [f"media-evidence-observe: process {counter} unavailable: {cause}"
                                 for counter, cause in expected])

    def test_readable_sources_give_every_counter_and_no_cause(self):
        usage, lines = self.usage(self.complete())
        self.assertEqual(usage, {"cpu_seconds": round(200 / os.sysconf("SC_CLK_TCK"), 3),
                                 "peak_rss_bytes": 2048 * 1024, "rchar": 5000,
                                 "read_bytes": 4096})
        self.assertEqual(lines, [])

    def test_a_host_without_proc_says_so_for_every_counter(self):
        usage, lines = self.usage(make_root=False)
        self.assertEqual(set(usage.values()), {observe.UNAVAILABLE})
        self.assert_causes(lines, [(counter, "no /proc on this host")
                                   for counter in ("cpu_seconds", "peak_rss_bytes", "rchar",
                                                   "read_bytes")])

    def test_a_missing_process_reports_the_read_error_per_source(self):
        usage, lines = self.usage()
        self.assertEqual(set(usage.values()), {observe.UNAVAILABLE})
        self.assert_causes(lines, [("cpu_seconds", "stat: FileNotFoundError"),
                                   ("peak_rss_bytes", "status: FileNotFoundError"),
                                   ("rchar", "io: FileNotFoundError"),
                                   ("read_bytes", "io: FileNotFoundError")])

    def test_a_bad_stat_leaves_only_cpu_unavailable(self):
        for stat_text, cause in (("123 (x) S 0 0 0 0 0 0 0 0 0 0 a b c d", "stat: ValueError"),
                                 ("123 x S", "stat: IndexError"),
                                 ("123 (x) S 0 0 0 0 0 0 0 0 0 0 1 1", "stat: field absent")):
            with self.subTest(stat=stat_text):
                usage, lines = self.usage(self.complete(stat=stat_text))
                self.assertEqual(usage["cpu_seconds"], observe.UNAVAILABLE)
                self.assertEqual((usage["peak_rss_bytes"], usage["rchar"], usage["read_bytes"]),
                                 (2048 * 1024, 5000, 4096))
                self.assert_causes(lines, [("cpu_seconds", cause)])

    def test_a_bad_status_leaves_only_peak_memory_unavailable(self):
        for status_text, cause in (("Name:\tx\n", "status: field absent"),
                                   ("VmHWM:\t x kB\n", "status: ValueError")):
            with self.subTest(status=status_text):
                usage, lines = self.usage(self.complete(status=status_text))
                self.assertEqual(usage["peak_rss_bytes"], observe.UNAVAILABLE)
                self.assertIsInstance(usage["cpu_seconds"], float)
                self.assert_causes(lines, [("peak_rss_bytes", cause)])

    def test_a_bad_io_leaves_only_its_counters_unavailable(self):
        usage, lines = self.usage(self.complete(io="rchar: 5000\n"))
        self.assertEqual((usage["rchar"], usage["read_bytes"]), (5000, observe.UNAVAILABLE))
        self.assert_causes(lines, [("read_bytes", "io: field absent")])
        usage, lines = self.usage(self.complete(io="rchar 5000\n"))
        self.assertEqual((usage["rchar"], usage["read_bytes"]), (observe.UNAVAILABLE,) * 2)
        self.assert_causes(lines, [("rchar", "io: ValueError"), ("read_bytes", "io: ValueError")])


class BufferTests(unittest.TestCase):
    def test_every_path_reads_1m_today_and_the_pattern_reads_the_16k_variant(self):
        for name in observe.OBSERVE_SOURCES:
            with self.subTest(path=name):
                self.assertEqual(observe.buffer_bytes(name), MIB)
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            source = root / observe.OBSERVE_SOURCES["ffprobe"]
            source.parent.mkdir(parents=True)
            source.write_text(observe.candidates.OBSERVE_16K["ffprobe"])
            self.assertEqual(observe.buffer_bytes("ffprobe", root), 16 * 1024)

    def test_default_concurrency_reads_only_max_in_flight_so_the_1m_variant_runs(self):
        self.assertEqual(observe.default_concurrency(), 4)
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            relative, _ = observe.baseline.SOURCE_CONSTANTS["max_in_flight"]
            (root / relative).parent.mkdir(parents=True)
            (root / relative).write_text("const MAX_IN_FLIGHT: usize = 4;\n")
            for name, path in observe.OBSERVE_SOURCES.items():
                source = root / path
                source.parent.mkdir(parents=True, exist_ok=True)
                source.write_text(observe.candidates.OBSERVE_1M)
            self.assertEqual(observe.default_concurrency(root), 4)


def valid_record():
    row = {"id": "f0001", "size_bytes": 3, "ext": ".mkv", "resident_bytes_before": 0,
           "queue_delay_seconds": 0.0, "observe_seconds": 1.0, "outcome": "observed",
           "failure": None}
    return {
        "schema": observe.SCHEMA,
        "corpus": {"label": "local-corpus", "storage_class": "local", "filesystem_type": "btrfs",
                   "manifest_files": 1, "selected_ids": ["f0001"], "skipped_budget_ids": []},
        "build": {"git_commit": "a" * 40, "git_changed_paths": [], "harness_sha256": "b" * 64,
                  "example_sha256": "c" * 64},
        "host": {"os": "Linux", "machine": "x86_64", "cpu_count": 1, "memory_bytes": 1,
                 "python": "3"},
        "parameters": {"path": "ffmpeg", "observe_buffer_bytes": 16384, "concurrency": 4,
                       "max_bytes_per_repetition": 10, "repetitions": 1, "evict": True},
        "repetitions": [{
            "index": 1, "evicted": True, "wall_seconds": 1.0, "logical_bytes_read": 3,
            "hash_set_sha256": "d" * 64,
            "process": {"cpu_seconds": 1.0, "peak_rss_bytes": 1, "rchar": 3, "read_bytes": 3},
            "host_counters": {"network_rx_bytes": 0, "cifs_bytes_read": "unavailable"},
            "files": [row],
        }],
        "not_measured": dict(observe.NOT_MEASURED),
    }


class ValidateTests(unittest.TestCase):
    def test_a_complete_record_is_valid(self):
        self.assertEqual(observe.validate_run(valid_record()), [])

    def test_each_contract_violation_is_reported(self):
        cases = {
            "schema": lambda r: r.update(schema="media-evidence-baseline-run/1"),
            "parameters path": lambda r: r["parameters"].update(path="hash"),
            "build example_sha256": lambda r: r["build"].pop("example_sha256"),
            "repetition hash_set_sha256":
                lambda r: r["repetitions"][0].update(hash_set_sha256="nope"),
            "process cpu_seconds":
                lambda r: r["repetitions"][0]["process"].update(cpu_seconds=-1),
            "file outcome": lambda r: r["repetitions"][0]["files"][0].update(outcome="hashed"),
            "file failure": lambda r: r["repetitions"][0]["files"][0].update(failure="x"),
            "host path": lambda r: r["corpus"].update(label="/home/someone/media"),
            "per-file hash":
                lambda r: r["repetitions"][0]["files"][0].update(ext="blake3:" + "0" * 64),
            "process read_bytes":
                lambda r: r["repetitions"][0]["process"].update(read_bytes=-1),
            "not_measured": lambda r: r["not_measured"].pop("operation"),
        }
        for problem, mutate in cases.items():
            with self.subTest(problem=problem):
                record = valid_record()
                mutate(record)
                self.assertIn(problem, observe.validate_run(record))

    def test_a_failed_row_needs_a_variant_name(self):
        record = valid_record()
        record["repetitions"][0]["files"][0].update(outcome="observe_failed", failure=None)
        self.assertIn("file failure", observe.validate_run(record))
        record["repetitions"][0]["files"][0]["failure"] = "artifact_unavailable"
        self.assertEqual(observe.validate_run(record), [])

    def test_committed_records_are_valid(self):
        for path in sorted(RECORD_DIR.glob("*.json")):
            with self.subTest(record=path.name):
                self.assertEqual(observe.validate_run(json.loads(path.read_text())), [])


def cell(buffer, walls, cpu, rss=2 * MIB, evict=True, path="ffmpeg", hash_set="d" * 64,
         outcome="observed", ids=("f0001",)):
    record = valid_record()
    record["corpus"]["selected_ids"] = list(ids)
    record["parameters"].update(path=path, observe_buffer_bytes=buffer, evict=evict)
    repetitions = []
    for index, (wall, seconds) in enumerate(zip(walls, cpu), start=1):
        repetition = json.loads(json.dumps(record["repetitions"][0]))
        repetition.update(index=index, wall_seconds=wall, hash_set_sha256=hash_set)
        repetition["process"].update(cpu_seconds=seconds, peak_rss_bytes=rss)
        repetition["files"][0].update(observe_seconds=wall, outcome=outcome)
        repetitions.append(repetition)
    record["repetitions"] = repetitions
    return record


def concurrency(record, value):
    record["parameters"]["concurrency"] = value
    return record


BASE = dict(buffer=16384, walls=[100.0, 100.0, 100.0], cpu=[50.0, 51.0, 49.0])


class CompareTests(unittest.TestCase):
    def test_cpu_improvement_without_regression_is_go(self):
        result = observe.compare(cell(**BASE), cell(MIB, [99.0, 100.0, 100.0], [20.0] * 3))
        self.assertEqual(result["metrics"]["cpu"]["verdict"], "improvement")
        self.assertEqual(result["verdict"], "go")

    def test_unchanged_cpu_is_inconclusive(self):
        result = observe.compare(cell(**BASE), cell(MIB, [100.0] * 3, [50.0] * 3))
        self.assertEqual(result["verdict"], "inconclusive")

    def test_a_wall_regression_is_no_go_despite_cpu(self):
        result = observe.compare(cell(**BASE), cell(MIB, [120.0] * 3, [20.0] * 3))
        self.assertEqual(result["metrics"]["wall"]["verdict"], "regression")
        self.assertEqual(result["verdict"], "no-go")

    def test_a_failed_row_or_differing_hashes_is_no_go(self):
        for candidate in (cell(MIB, [99.0] * 3, [20.0] * 3, outcome="observe_failed"),
                          cell(MIB, [99.0] * 3, [20.0] * 3, hash_set="e" * 64)):
            with self.subTest():
                self.assertEqual(observe.compare(cell(**BASE), candidate)["verdict"], "no-go")

    def test_memory_beyond_the_guardrail_is_no_go(self):
        result = observe.compare(cell(**BASE), cell(MIB, [99.0] * 3, [20.0] * 3, rss=300 * MIB))
        self.assertEqual(result["guardrails"], ["peak memory"])
        self.assertEqual(result["verdict"], "no-go")

    def test_as_found_compares_repetitions_after_the_first(self):
        base = cell(16384, [300.0, 100.0, 100.0], [50.0, 50.0, 51.0], evict=False)
        cand = cell(MIB, [10.0, 100.0, 100.0], [20.0, 20.0, 20.0], evict=False)
        wall = observe.compare(base, cand)["metrics"]["wall"]
        self.assertEqual((wall["baseline_first"], wall["candidate_first"]), (300.0, 10.0))
        self.assertEqual(wall["baseline_median"], 100.0)

    def test_an_observe_seconds_regression_alone_is_no_go(self):
        candidate = cell(MIB, [100.0] * 3, [20.0] * 3)
        for repetition in candidate["repetitions"]:
            repetition["files"][0]["observe_seconds"] = 130.0
        result = observe.compare(cell(**BASE), candidate)
        self.assertEqual(result["metrics"]["wall"]["verdict"], "inconclusive")
        self.assertEqual(result["metrics"]["observe_seconds"]["verdict"], "regression")
        self.assertEqual(result["verdict"], "no-go")

    def test_cpu_beyond_half_again_without_a_quarter_wall_gain_breaches_the_guardrail(self):
        result = observe.compare(cell(**BASE), cell(MIB, [95.0] * 3, [80.0] * 3))
        self.assertEqual(result["guardrails"], ["cpu"])
        self.assertEqual(result["verdict"], "no-go")

    def test_the_hash_set_digest_ignores_completion_order(self):
        lines = ["f0002 blake3:bb", "f0001 blake3:aa", "f0003 blake3:cc"]
        self.assertEqual(observe.hash_set_sha256(lines),
                         observe.hash_set_sha256(list(reversed(lines))))

    def test_mismatched_cells_are_refused(self):
        for candidate in (cell(MIB, [99.0] * 3, [20.0] * 3, path="ffprobe"),
                          cell(MIB, [99.0] * 3, [20.0] * 3, evict=False),
                          cell(MIB, [99.0] * 3, [20.0] * 3, ids=("f0002",)),
                          concurrency(cell(MIB, [99.0] * 3, [20.0] * 3), 2),
                          cell(16384, [99.0] * 3, [20.0] * 3)):
            with self.subTest(), self.assertRaises(observe.HarnessError):
                observe.compare(cell(**BASE), candidate)


if __name__ == "__main__":
    unittest.main()
