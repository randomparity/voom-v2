#!/usr/bin/env python3
"""Prove the media evidence harness records required fields without leaking host paths.

A stub worker speaks the worker HTTP protocol so the replay runs on any CI host
without media tools or built workers; real-protocol evidence is the committed
baseline runs, which are validated here too.
"""

import argparse
import concurrent.futures
import contextlib
import importlib.util
import io
import json
import os
import re
import socket
import stat
import subprocess
import tempfile
import threading
import time
import unittest
from unittest import mock
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "media_evidence", Path(__file__).with_name("media-evidence-baseline.py")
)
harness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(harness)

BASELINE_DIR = harness.REPO_ROOT / "docs" / "media-evidence-baseline"

STUB = r'''#!/usr/bin/env python3
import hashlib, json, os, sys, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PROBE = os.path.basename(sys.argv[0]) == "voom-ffprobe-worker"

def facts(path):
    data = open(path, "rb").read()
    digest = "blake3:" + hashlib.sha256(data).hexdigest()
    return {"size_bytes": len(data), "content_hash": digest, "modified_at": "2026-01-01T00:00:00Z"}

def hash_facts(path):
    # The real hash worker reports the guard inputs; "nokey" files model a missing file key.
    result = dict(facts(path), changed_at_ns=1, file_key={"dev": 1, "ino": 2, "nlink": 1})
    if "nokey" in os.path.basename(path):
        result["file_key"] = None
    return result

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        if self.headers["Authorization"] != "Bearer " + os.environ["VOOM_WORKER_SECRET"]:
            self.send_response(401); self.end_headers(); return
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        payload, lease = request["payload"], request["lease_id"]
        path = payload["path"] if PROBE else os.path.join(
            payload["provider_locator"], payload["provider_relative_locator"])
        frames = [{"kind": "progress", "lease_id": lease, "seq": 0}]
        name = os.path.basename(path)
        if "trunc" in name and not PROBE:
            self.send_response(200)
            self.send_header("Content-Length", "100000")
            self.end_headers()
            self.wfile.write(b'{"lease_id": 1}\n')
            return
        if "fail" in name:
            frames.append({"kind": "error", "class": "artifact_unavailable", "code": "IO",
                           "message": "cannot read " + path})
        elif "weird" in name and PROBE:
            frames.append({"kind": "surprise"})
        elif "mismatch" in name and PROBE and "guard" in payload:
            frames.append({"kind": "error", "class": "artifact_checksum_mismatch",
                           "code": "ARTIFACT_CHECKSUM_MISMATCH", "message": "guard " + path})
        elif PROBE and "guard" in payload:
            # A guarded probe echoes the expected hash without reading the file (ADR 0120).
            echoed = {key: payload["expected"][key]
                      for key in ("size_bytes", "content_hash", "modified_at")}
            frames.append({"kind": "result", "payload": {
                "pre_probe": echoed, "post_probe": echoed, "snapshot": {},
                "hash_source": "reused"}})
        elif "badpayload" in name and PROBE:
            frames.append({"kind": "result", "payload": {"pre_probe": {}}})
        elif PROBE:
            observed = facts(path)
            frames.append({"kind": "result", "payload": {
                "pre_probe": observed, "post_probe": observed, "snapshot": {}}})
        else:
            frames.append({"kind": "result", "payload": hash_facts(path)})
        body = "\n".join(json.dumps(item) for item in [{"lease_id": lease}, *frames]) + "\n"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body.encode())

server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print("BOUND addr=127.0.0.1:%d" % server.server_address[1], flush=True)
threading.Thread(target=server.serve_forever, daemon=True).start()
sys.stdin.read()
server.shutdown()
'''


class HarnessFixture(unittest.TestCase):
    NAMES = {"a/Alpha Title (2001).mkv": 64, "b/fail-Beta.mp4": 32,
             "c/weird-Gamma.mkv": 16, "d/trunc-Delta.mkv": 8,
             "e/badpayload-Epsilon.mkv": 8, "notes.txt": 8}

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.root = self.base / "secret-library-root"
        names = self.NAMES
        for relative, size in names.items():
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"x" * size)
        self.names = [Path(relative).stem for relative in names]
        self.workers = self.base / "workers"
        self.workers.mkdir()
        for name in ("voom-hash-worker", "voom-ffprobe-worker"):
            stub = self.workers / name
            stub.write_text(STUB, encoding="utf-8")
            stub.chmod(stub.stat().st_mode | stat.S_IXUSR)
        self.manifest_path = self.base / "manifest.json"
        manifest = harness.build_manifest(self.root, "fixture-corpus")
        self.manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

    def run_args(self, max_bytes=10_000, evict=False, probe_reuse="off"):
        return argparse.Namespace(
            manifest=self.manifest_path, worker_dir=self.workers, ids=None,
            max_bytes=max_bytes, repetitions=2, concurrency=None, evict=evict,
            probe_reuse=probe_reuse,
        )

    def assert_redacted(self, record):
        text = json.dumps(record)
        self.assertNotIn(str(self.base), text)
        for name in self.names:
            self.assertNotIn(name, text)


class ManifestTests(HarnessFixture):
    def test_manifest_lists_only_video_files_by_ordinal_id(self):
        manifest = json.loads(self.manifest_path.read_text(encoding="utf-8"))
        self.assertEqual([entry["id"] for entry in manifest["files"]],
                         ["f0001", "f0002", "f0003", "f0004", "f0005"])
        self.assertEqual([entry["ext"] for entry in manifest["files"]],
                         [".mkv", ".mp4", ".mkv", ".mkv", ".mkv"])

    def test_label_must_be_public_safe(self):
        with self.assertRaises(harness.HarnessError):
            harness.build_manifest(self.root, str(self.root))

    def test_private_manifest_is_refused_inside_a_checkout_outside_agent_dir(self):
        with self.assertRaises(harness.HarnessError):
            harness.refuse_private_path_in_checkout(harness.REPO_ROOT / "docs" / "m.json")
        harness.refuse_private_path_in_checkout(self.base / "elsewhere" / "m.json")

    def test_private_manifest_is_allowed_in_a_checkout_only_where_git_ignores_it(self):
        repo = self.base / "clone"
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        with self.assertRaises(harness.HarnessError):
            harness.refuse_private_path_in_checkout(repo / ".agent" / "m.json")
        (repo / ".gitignore").write_text(".agent/\n")
        harness.refuse_private_path_in_checkout(repo / ".agent" / "m.json")


class ReplayTests(HarnessFixture):
    def test_replay_records_fields_failures_and_leaves_media_unchanged(self):
        before = {path: path.stat().st_mtime_ns for path in self.root.rglob("*") if path.is_file()}
        record = harness.capture(self.run_args())
        after = {path: path.stat().st_mtime_ns for path in self.root.rglob("*") if path.is_file()}
        self.assertEqual(before, after)
        self.assertEqual(harness.validate_run(record), [])
        self.assert_redacted(record)
        self.assertEqual(record["parameters"]["concurrency"], record["parameters"]["max_in_flight"])
        self.assertEqual(len(record["repetitions"]), 2)
        rows = {row["id"]: row for row in record["repetitions"][0]["files"]}
        self.assertEqual(rows["f0001"]["outcome"], "observed")
        self.assertEqual(rows["f0002"]["outcome"], "hash_failed")
        self.assertEqual(rows["f0002"]["failure"],
                         {"stage": "hash", "class": "artifact_unavailable", "code": "IO"})
        self.assertEqual(rows["f0003"]["outcome"], "probe_failed")
        self.assertEqual(rows["f0003"]["failure"]["code"], "unknown_frame")
        self.assertEqual(rows["f0004"]["outcome"], "hash_failed")
        self.assertEqual(rows["f0004"]["failure"]["code"], "transport_IncompleteRead")
        self.assertEqual(rows["f0005"]["outcome"], "probe_failed")
        self.assertEqual(rows["f0005"]["failure"]["code"], "malformed_result")
        # Completed whole-file reads only: observed 3, probe_failed 1 (hash), hash_failed 0.
        self.assertEqual(record["repetitions"][0]["logical_bytes_read"], 64 * 3 + 16 + 8)

    def test_off_records_three_reads_and_recomputed_sources(self):
        record = harness.capture(self.run_args())
        self.assertEqual(record["schema"], harness.SCHEMA_V2)
        self.assertIs(record["parameters"]["probe_hash_reuse"], False)
        rows = {row["id"]: row for row in record["repetitions"][0]["files"]}
        self.assertEqual(rows["f0001"]["hash_source"], "recomputed")
        # No probe result, no source: hash failures and probe failures alike.
        for file_id in ("f0002", "f0003", "f0004", "f0005"):
            self.assertIsNone(rows[file_id]["hash_source"])

    def test_budget_stops_scheduling_files_that_do_not_fit(self):
        record = harness.capture(self.run_args(max_bytes=64 * 3 + 16 * 3))
        self.assertEqual(record["corpus"]["selected_ids"], ["f0001", "f0003"])
        self.assertEqual(record["corpus"]["skipped_budget_ids"], ["f0002", "f0004", "f0005"])

    def test_nothing_fitting_the_budget_is_refused(self):
        with self.assertRaises(harness.HarnessError):
            harness.capture(self.run_args(max_bytes=1))

    def test_selected_files_spanning_filesystems_are_refused(self):
        manifest = json.loads(self.manifest_path.read_text(encoding="utf-8"))
        real = harness.filesystem_type
        harness.filesystem_type = lambda path: "cifs" if "trunc" in path.name else "btrfs"
        self.addCleanup(setattr, harness, "filesystem_type", real)
        with self.assertRaisesRegex(harness.HarnessError, "span filesystems"):
            harness.corpus_filesystem(self.root, manifest["files"])
        self.assertEqual(harness.corpus_filesystem(self.root, manifest["files"][:2]), "btrfs")

    def test_effective_mount_is_the_last_longest_match_with_escapes_decoded(self):
        mounts = self.base / "mounts"
        mounts.write_text(
            "/dev/a / btrfs rw 0 0\n"
            "systemd-1 /srv/share autofs rw 0 0\n"
            "//nas/media /srv/share cifs rw 0 0\n"
            "/dev/b /srv/tab\\011dir ext4 rw 0 0\n"
        )
        self.assertEqual(harness.filesystem_type(Path("/srv/share/x.mkv"), mounts), "cifs")
        self.assertEqual(harness.filesystem_type(Path("/srv/tab\tdir/x.mkv"), mounts), "ext4")
        self.assertEqual(harness.filesystem_type(Path("/srv/other.mkv"), mounts), "btrfs")

    def test_evict_refuses_non_local_storage(self):
        for fs_type in ("cifs", "nfs4", "autofs", "fuse.rclone", harness.UNAVAILABLE):
            with self.subTest(fs_type=fs_type), self.assertRaises(harness.HarnessError):
                harness.evict(self.root / "a" / "Alpha Title (2001).mkv", fs_type)


class ReuseTests(HarnessFixture):
    NAMES = {"a/Alpha.mkv": 64, "b/nokey-Beta.mkv": 32, "c/mismatch-Gamma.mkv": 16}

    def test_reuse_on_sends_the_guard_only_with_ctime_and_file_key(self):
        record = harness.capture(self.run_args(probe_reuse="on"))
        self.assertEqual(harness.validate_run(record), [])
        self.assert_redacted(record)
        self.assertIs(record["parameters"]["probe_hash_reuse"], True)
        rows = {row["id"]: row for row in record["repetitions"][0]["files"]}
        self.assertEqual((rows["f0001"]["outcome"], rows["f0001"]["hash_source"]),
                         ("observed", "reused"))
        # Without a file key no guard can be sent, so the probe recomputes.
        self.assertEqual((rows["f0002"]["outcome"], rows["f0002"]["hash_source"]),
                         ("observed", "recomputed"))
        # A guard mismatch is an error frame: no probe result, so no source.
        self.assertEqual(rows["f0003"]["outcome"], "probe_failed")
        self.assertIsNone(rows["f0003"]["hash_source"])
        self.assertEqual(rows["f0003"]["failure"]["code"], "ARTIFACT_CHECKSUM_MISMATCH")
        # Reused 1 read, recomputed 3, probe_failed 1 (the hash pass).
        self.assertEqual(record["repetitions"][0]["logical_bytes_read"], 64 + 32 * 3 + 16)

    def test_reuse_off_never_sends_the_guard(self):
        record = harness.capture(self.run_args())
        rows = record["repetitions"][0]["files"]
        self.assertEqual({row["hash_source"] for row in rows}, {"recomputed"})
        self.assertEqual(record["repetitions"][0]["logical_bytes_read"], (64 + 32 + 16) * 3)


class StalledWorkerTests(HarnessFixture):
    def setUp(self):
        super().setUp()
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        listener.settimeout(0.05)
        finished = threading.Event()
        self.accepted = []

        def accept_requests():
            while not finished.is_set():
                try:
                    connection, _address = listener.accept()
                except TimeoutError:
                    continue
                self.accepted.append(connection)

        thread = threading.Thread(target=accept_requests)
        thread.start()

        def cleanup():
            finished.set()
            thread.join(timeout=5)
            listener.close()
            for connection in self.accepted:
                connection.close()
            self.assertFalse(thread.is_alive(), "stalled listener did not stop")

        self.addCleanup(cleanup)
        self.worker = object.__new__(harness.Worker)
        self.worker.host, self.worker.port = listener.getsockname()
        self.worker.secret, self.worker.lease = "secret", 0
        self.worker.lock, self.worker.abandoned = threading.Lock(), False
        self.worker.process = mock.Mock(pid=os.getpid())
        for name, value in (("PROGRESS_IDLE_DEADLINE_MS", 100),
                            ("CALL_TIMEOUT_MARGIN_SECONDS", 0.1)):
            patch = mock.patch.object(harness, name, value)
            patch.start()
            self.addCleanup(patch.stop)

    def test_timeout_abandons_later_calls_without_another_request(self):
        started = time.monotonic()
        self.assertEqual(self.worker.call("hash", {}),
                         {"kind": "failure", "code": "transport_timeout"})
        self.assertEqual(self.worker.call("hash", {}),
                         {"kind": "failure", "code": "worker_abandoned"})
        self.assertLess(time.monotonic() - started, 5)
        self.assertEqual(len(self.accepted), 1)
        self.assertEqual(self.worker.lease, 1)

    def test_timeout_withholds_affected_worker_usage(self):
        self.assertEqual(self.worker.call("hash", {})["code"], "transport_timeout")
        self.assertEqual(self.worker.usage(), dict.fromkeys(harness.WORKER_KEYS,
                                                          harness.UNAVAILABLE))

    def test_repetition_marks_dependent_rows_and_only_the_affected_worker_usage(self):
        original_worker = harness.Worker
        for role in ("hash", "probe"):
            with self.subTest(role=role):
                self.worker.abandoned = False
                self.worker.lease = 0
                healthy = original_worker(self.workers / (
                    "voom-ffprobe-worker" if role == "hash" else "voom-hash-worker"))
                self.addCleanup(healthy.stop)
                workers = [self.worker, healthy] if role == "hash" else [healthy, self.worker]
                manifest = json.loads(self.manifest_path.read_text())
                files = [dict(manifest["files"][0], id=f"f{index:04d}")
                         for index in (1, 2)]
                args = self.run_args()
                args.concurrency = 1
                with mock.patch.object(harness, "Worker", side_effect=workers), \
                        mock.patch.object(self.worker, "stop") as stop:
                    repetition = harness.run_repetition(1, manifest, files, args, "ext4")
                self.assertEqual([row["outcome"] for row in repetition["files"]],
                                 [f"{role}_failed"] * 2)
                self.assertEqual([row["failure"] for row in repetition["files"]], [
                    {"stage": role, "class": "harness", "code": "transport_timeout"},
                    {"stage": role, "class": "harness", "code": "worker_abandoned"},
                ])
                self.assertEqual(repetition["workers"][role],
                                 dict.fromkeys(harness.WORKER_KEYS, harness.UNAVAILABLE))
                self.assertEqual(self.worker.lease, 1)
                self.assertEqual(harness.repetition_problems(repetition, v2=True), [])
                stop.assert_called_once_with()
        self.assertEqual(len(self.accepted), 2)

    def test_in_flight_success_does_not_revive_an_abandoned_worker(self):
        entered, first_entered, release = (threading.Event() for _ in range(3))
        first, second, later = (mock.MagicMock() for _ in range(3))
        later.getresponse.side_effect = TimeoutError
        response = second.getresponse.return_value
        response.status = 200
        response.read.return_value = b'{"lease_id":2}\n{"kind":"result","payload":{}}\n'

        def timeout():
            first_entered.set()
            if not entered.wait(timeout=5):
                raise AssertionError("second request never entered transport")
            raise TimeoutError

        def complete():
            entered.set()
            if not release.wait(timeout=5):
                raise AssertionError("in-flight request was not released")
            return response

        first.getresponse.side_effect, second.getresponse.side_effect = timeout, complete
        with mock.patch.object(harness.http.client, "HTTPConnection",
                               side_effect=[first, second, later]) as connection:
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                try:
                    timed_out = pool.submit(self.worker.call, "hash", {})
                    self.assertTrue(first_entered.wait(timeout=5))
                    successful = pool.submit(self.worker.call, "hash", {})
                    self.assertEqual(timed_out.result(timeout=5)["code"], "transport_timeout")
                    self.assertEqual(self.worker.call("hash", {})["code"], "worker_abandoned")
                finally:
                    release.set()
                self.assertEqual(successful.result(timeout=5)["kind"], "result")
            self.assertEqual(connection.call_count, 2)
        self.assertEqual(self.worker.usage(), dict.fromkeys(harness.WORKER_KEYS,
                                                          harness.UNAVAILABLE))

    def test_a_call_queued_on_the_lock_observes_abandonment(self):
        entered = threading.Event()

        def call():
            entered.set()
            return self.worker.call("hash", {})

        with mock.patch.object(harness.http.client, "HTTPConnection") as connection:
            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                with self.worker.lock:
                    queued = pool.submit(call)
                    self.assertTrue(entered.wait(timeout=5))
                    self.worker.abandoned = True
                self.assertEqual(queued.result(timeout=5)["code"], "worker_abandoned")
            connection.assert_not_called()

    def test_other_transport_errors_keep_their_error_class_name(self):
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            host, port = listener.getsockname()
        self.worker.host, self.worker.port = host, port
        self.assertEqual(self.worker.call("hash", {}),
                         {"kind": "failure", "code": "transport_ConnectionRefusedError"})
        self.assertFalse(self.worker.abandoned)


class ProcUsageTests(unittest.TestCase):
    """`unavailable` stays a valid value; stderr names each unavailable counter's cause."""

    STAT = "123 (observe ffmpeg) S" + " 0" * 10 + " 100 50 25 25 0 0"
    STATUS = "Name:\tobserve-ffmpeg\nVmHWM:\t    2048 kB\n"
    IO = "rchar: 5000\nwchar: 0\nread_bytes: 4096\n"
    COUNTERS = ("cpu_seconds", "peak_rss_bytes", "rchar", "read_bytes")

    def usage(self, sources=None, make_root=True, read=("stat", "status", "io")):
        """`proc_usage` over a scratch /proc holding `sources` for pid 123."""
        with tempfile.TemporaryDirectory() as scratch:
            proc = Path(scratch) / "proc"
            if make_root:
                proc.mkdir()
            if sources is not None:
                (proc / "123").mkdir()
                for name, text in sources.items():
                    (proc / "123" / name).write_text(text)
            stderr = io.StringIO()
            with mock.patch.object(harness, "PROC_ROOT", proc), \
                    contextlib.redirect_stderr(stderr):
                usage = harness.proc_usage(123, "media-evidence-test", read)
        return usage, stderr.getvalue().splitlines()

    def complete(self, **overrides):
        return {"stat": self.STAT, "status": self.STATUS, "io": self.IO, **overrides}

    def assert_causes(self, lines, expected):
        self.assertEqual(lines, [f"media-evidence-test: process {counter} unavailable: {cause}"
                                 for counter, cause in expected])

    def test_readable_sources_give_every_counter_and_no_cause(self):
        usage, lines = self.usage(self.complete())
        self.assertEqual(usage, {"cpu_seconds": round(200 / os.sysconf("SC_CLK_TCK"), 3),
                                 "peak_rss_bytes": 2048 * 1024, "rchar": 5000,
                                 "read_bytes": 4096})
        self.assertEqual(lines, [])

    def test_a_host_without_proc_says_so_for_every_counter(self):
        usage, lines = self.usage(make_root=False)
        self.assertEqual(usage, dict.fromkeys(self.COUNTERS, harness.UNAVAILABLE))
        self.assert_causes(lines, [(counter, "no /proc on this host")
                                   for counter in self.COUNTERS])

    def test_a_missing_process_reports_the_read_error_per_source(self):
        usage, lines = self.usage()
        self.assertEqual(usage, dict.fromkeys(self.COUNTERS, harness.UNAVAILABLE))
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
                self.assertEqual(usage["cpu_seconds"], harness.UNAVAILABLE)
                self.assertEqual((usage["peak_rss_bytes"], usage["rchar"], usage["read_bytes"]),
                                 (2048 * 1024, 5000, 4096))
                self.assert_causes(lines, [("cpu_seconds", cause)])

    def test_a_bad_status_leaves_only_peak_memory_unavailable(self):
        for status_text, cause in (("Name:\tx\n", "status: field absent"),
                                   ("VmHWM:\t x kB\n", "status: ValueError")):
            with self.subTest(status=status_text):
                usage, lines = self.usage(self.complete(status=status_text))
                self.assertEqual(usage["peak_rss_bytes"], harness.UNAVAILABLE)
                self.assertIsInstance(usage["cpu_seconds"], float)
                self.assert_causes(lines, [("peak_rss_bytes", cause)])

    def test_a_bad_io_leaves_only_its_counters_unavailable(self):
        usage, lines = self.usage(self.complete(io="rchar: 5000\n"))
        self.assertEqual((usage["rchar"], usage["read_bytes"]), (5000, harness.UNAVAILABLE))
        self.assert_causes(lines, [("read_bytes", "io: field absent")])
        usage, lines = self.usage(self.complete(io="rchar 5000\n"))
        self.assertEqual((usage["rchar"], usage["read_bytes"]), (harness.UNAVAILABLE,) * 2)
        self.assert_causes(lines, [("rchar", "io: ValueError"), ("read_bytes", "io: ValueError")])

    def test_only_the_selected_sources_are_read(self):
        usage, lines = self.usage(self.complete(stat="unreadable"), read=("status", "io"))
        self.assertEqual(usage, {"peak_rss_bytes": 2048 * 1024, "rchar": 5000,
                                 "read_bytes": 4096})
        self.assertEqual(lines, [])


class HostCountersTests(unittest.TestCase):
    """`unavailable` stays a valid value; stderr names each unavailable counter's cause, except
    a missing CIFS stats file, which only means the host has no CIFS client to count."""

    NET_DEV = ("Inter-|   Receive\n face |bytes    packets\n"
               "    lo: 999 1 0 0 0 0 0 0 999 1\n"
               "  eth0: 1000 5 0 0 0 0 0 0 10 1\n"
               "  wlan0: 24 1 0 0 0 0 0 0 10 1\n")
    CIFS = "Resources in use\n1) \\\\nas\\media\nBytes read: 700  Bytes written: 1\n" \
           "2) \\\\nas\\other\nBytes read: 77  Bytes written: 1\n"

    def counters(self, files=None, make_root=True):
        """`host_counters` over a scratch /proc holding `files` (relative path to text)."""
        with tempfile.TemporaryDirectory() as scratch:
            proc = Path(scratch) / "proc"
            if make_root:
                proc.mkdir()
            for name, text in (files or {}).items():
                (proc / name).parent.mkdir(parents=True, exist_ok=True)
                (proc / name).write_text(text)
            stderr = io.StringIO()
            with mock.patch.object(harness, "PROC_ROOT", proc), \
                    contextlib.redirect_stderr(stderr):
                counters = harness.host_counters("media-evidence-test")
        return counters, stderr.getvalue().splitlines()

    def assert_causes(self, lines, expected):
        self.assertEqual(lines, [f"media-evidence-test: host {counter} unavailable: {cause}"
                                 for counter, cause in expected])

    def test_readable_files_give_both_counters_and_no_cause(self):
        counters, lines = self.counters({"net/dev": self.NET_DEV, "fs/cifs/Stats": self.CIFS})
        self.assertEqual(counters, {"network_rx_bytes": 1024, "cifs_bytes_read": 777})
        self.assertEqual(lines, [])

    def test_a_host_without_proc_says_so_for_both_counters(self):
        counters, lines = self.counters(make_root=False)
        self.assertEqual(counters, dict.fromkeys(harness.COUNTER_KEYS, harness.UNAVAILABLE))
        self.assert_causes(lines, [("network_rx_bytes", "no /proc on this host"),
                                   ("cifs_bytes_read", "no /proc on this host")])

    def test_a_host_without_a_cifs_client_stays_quiet_about_cifs(self):
        counters, lines = self.counters({"net/dev": self.NET_DEV})
        self.assertEqual(counters, {"network_rx_bytes": 1024,
                                    "cifs_bytes_read": harness.UNAVAILABLE})
        self.assertEqual(lines, [])

    def test_an_unreadable_cifs_file_names_its_error(self):
        counters, lines = self.counters({"net/dev": self.NET_DEV, "fs/cifs/Stats/x": ""})
        self.assertEqual(counters["cifs_bytes_read"], harness.UNAVAILABLE)
        self.assert_causes(lines, [("cifs_bytes_read", "fs/cifs/Stats: IsADirectoryError")])

    def test_a_missing_or_bad_network_file_names_its_error(self):
        cifs = {"fs/cifs/Stats": self.CIFS}
        for files, cause in (({}, "net/dev: FileNotFoundError"),
                             ({"net/dev": "h\nh\n  eth0 1000 5\n"}, "net/dev: IndexError"),
                             ({"net/dev": "h\nh\n  eth0: x 5\n"}, "net/dev: ValueError")):
            with self.subTest(cause=cause):
                counters, lines = self.counters({**files, **cifs})
                self.assertEqual(counters, {"network_rx_bytes": harness.UNAVAILABLE,
                                            "cifs_bytes_read": 777})
                self.assert_causes(lines, [("network_rx_bytes", cause)])


class WorkerUsageTests(unittest.TestCase):
    """The worker sampler reads each /proc source on its own and never samples an abandoned
    worker, whose cause the row failure `worker_abandoned` already records."""

    def sample(self, abandoned):
        worker = object.__new__(harness.Worker)
        worker.lock, worker.abandoned = threading.Lock(), abandoned
        worker.process = mock.Mock(pid=123)
        with tempfile.TemporaryDirectory() as scratch:
            proc = Path(scratch)
            (proc / "123").mkdir()
            for name, text in (("stat", "123 x S"), ("status", ProcUsageTests.STATUS),
                               ("io", ProcUsageTests.IO)):
                (proc / "123" / name).write_text(text)
            stderr = io.StringIO()
            with mock.patch.object(harness, "PROC_ROOT", proc), \
                    contextlib.redirect_stderr(stderr):
                usage = worker.usage()
        return usage, stderr.getvalue().splitlines()

    def test_a_bad_stat_keeps_the_other_counters_and_names_its_cause(self):
        usage, lines = self.sample(abandoned=False)
        self.assertEqual(usage, {"cpu_seconds": harness.UNAVAILABLE,
                                 "peak_rss_bytes": 2048 * 1024, "rchar": 5000,
                                 "read_bytes": 4096})
        self.assertEqual(lines, ["media-evidence-baseline: process cpu_seconds unavailable: "
                                 "stat: IndexError"])

    def test_an_abandoned_worker_is_not_sampled(self):
        usage, lines = self.sample(abandoned=True)
        self.assertEqual(usage, dict.fromkeys(harness.WORKER_KEYS, harness.UNAVAILABLE))
        self.assertEqual(lines, [])


class ContractTests(unittest.TestCase):
    def test_source_constants_match_current_behavior(self):
        constants = harness.read_constants(harness.REPO_ROOT)
        self.assertEqual(constants["max_in_flight"], 4)
        self.assertEqual(constants["flush_observation_count"], 1000)

    def test_offered_protocol_version_is_the_checked_out_constant(self):
        # Workers refuse any other version (ADR 0016); a hard-coded value went stale at #811.
        source = (harness.REPO_ROOT / "crates/voom-core/src/lib.rs").read_text(encoding="utf-8")
        declared = re.search(r"^pub const PROTOCOL_VERSION: u32 = (\d+);$", source, re.M)
        self.assertIsNotNone(declared)
        self.assertEqual(harness.protocol_version(), declared.group(1))

    def test_renamed_source_constant_fails_instead_of_recording_a_stale_value(self):
        with tempfile.TemporaryDirectory() as temporary:
            for relative, _pattern in harness.SOURCE_CONSTANTS.values():
                copy = Path(temporary, relative)
                copy.parent.mkdir(parents=True, exist_ok=True)
                copy.write_text((harness.REPO_ROOT / relative).read_text(encoding="utf-8"))
            hash_source = Path(temporary, "crates/voom-hash-worker/src/hash.rs")
            renamed = hash_source.read_text().replace("HASH_CHUNK_BYTES", "READ_CHUNK_BYTES")
            hash_source.write_text(renamed)
            with self.assertRaisesRegex(harness.HarnessError, "hash_chunk_bytes not found"):
                harness.read_constants(Path(temporary))

    def test_changed_paths_keep_every_character_of_porcelain_paths(self):
        porcelain = " M justfile\0?? docs/new file.md\0R  new.py\0old.py\0"
        self.assertEqual(harness.changed_paths(porcelain),
                         ["docs/new file.md", "justfile", "new.py"])

    def test_build_identity_never_records_untracked_file_names(self):
        with tempfile.TemporaryDirectory() as temporary:
            repo = Path(temporary)
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            (repo / "tracked.txt").write_text("one")
            subprocess.run(["git", "-C", str(repo), "add", "tracked.txt"], check=True)
            (repo / "tracked.txt").write_text("two")
            (repo / "Alpha Title (2001).mkv").write_bytes(b"x")
            self.assertEqual(harness.tracked_changes(repo), ["tracked.txt"])

    def test_validation_rejects_host_paths_and_missing_metrics(self):
        self.assertIn("missing repetitions", harness.validate_run({"schema": harness.SCHEMA}))
        leaked = {"schema": harness.SCHEMA, "corpus": {"root": "/srv/media"}, "build": {},
                  "host": {}, "parameters": {}, "repetitions": [], "not_measured": {}}
        self.assertIn("host path", harness.validate_run(leaked))

    def test_validation_rejects_missing_identity_markers_and_failure_shape(self):
        path = sorted(BASELINE_DIR.glob("*.json"))[0]
        record = json.loads(path.read_text(encoding="utf-8"))
        del record["build"]["harness_sha256"]
        del record["not_measured"]["retries"]
        row = record["repetitions"][0]["files"][0]
        row["outcome"] = "hash_failed"
        problems = harness.validate_run(record)
        for expected in ("build harness_sha256", "not_measured", "file failure"):
            self.assertIn(expected, problems)
        del record["repetitions"][0]["workers"]["hash"]["rchar"]
        del record["repetitions"][0]["files"][1]["size_bytes"]
        record["repetitions"][0]["wall_seconds"] = -1
        problems = harness.validate_run(record)
        for expected in ("worker hash rchar", "file size_bytes", "repetition wall_seconds"):
            self.assertIn(expected, problems)
        del record["repetitions"][0]["workers"]
        self.assertIn("repetition workers", harness.validate_run(record))

    def test_v2_requires_the_reuse_setting_and_each_row_source(self):
        path = sorted(BASELINE_DIR.glob("*.json"))[0]
        record = json.loads(path.read_text(encoding="utf-8"))
        self.assertEqual(record["schema"], harness.SCHEMA)
        record["schema"] = harness.SCHEMA_V2
        problems = harness.validate_run(record)
        self.assertIn("parameters probe_hash_reuse", problems)
        self.assertIn("file hash_source", problems)
        record["parameters"]["probe_hash_reuse"] = "yes"
        for repetition in record["repetitions"]:
            for row in repetition["files"]:
                row["hash_source"] = "cached"
        problems = harness.validate_run(record)
        self.assertIn("parameters probe_hash_reuse", problems)
        self.assertIn("file hash_source", problems)
        record["parameters"]["probe_hash_reuse"] = True
        for repetition in record["repetitions"]:
            for row in repetition["files"]:
                row["hash_source"] = "reused"
        self.assertEqual(harness.validate_run(record), [])

    def test_committed_baseline_runs_satisfy_the_record_contract(self):
        runs = sorted([*BASELINE_DIR.glob("*.json"), *BASELINE_DIR.glob("concurrency/*.json")])
        self.assertTrue(runs, "no committed baseline runs")
        for path in runs:
            with self.subTest(run=path.name):
                record = json.loads(path.read_text(encoding="utf-8"))
                self.assertEqual(harness.validate_run(record), [])


if __name__ == "__main__":
    unittest.main()
