#!/usr/bin/env python3
"""Prove the media evidence harness records required fields without leaking host paths.

A stub worker speaks the worker HTTP protocol so the replay runs on any CI host
without media tools or built workers; real-protocol evidence is the committed
baseline runs, which are validated here too.
"""

import argparse
import importlib.util
import json
import stat
import subprocess
import tempfile
import unittest
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
        elif "badpayload" in name and PROBE:
            frames.append({"kind": "result", "payload": {"pre_probe": {}}})
        elif PROBE:
            observed = facts(path)
            frames.append({"kind": "result", "payload": {
                "pre_probe": observed, "post_probe": observed, "snapshot": {}}})
        else:
            frames.append({"kind": "result", "payload": facts(path)})
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
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.root = self.base / "secret-library-root"
        names = {"a/Alpha Title (2001).mkv": 64, "b/fail-Beta.mp4": 32,
                 "c/weird-Gamma.mkv": 16, "d/trunc-Delta.mkv": 8,
                 "e/badpayload-Epsilon.mkv": 8, "notes.txt": 8}
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

    def run_args(self, max_bytes=10_000, evict=False):
        return argparse.Namespace(
            manifest=self.manifest_path, worker_dir=self.workers, ids=None,
            max_bytes=max_bytes, repetitions=2, concurrency=None, evict=evict,
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


class ContractTests(unittest.TestCase):
    def test_source_constants_match_current_behavior(self):
        constants = harness.read_constants(harness.REPO_ROOT)
        self.assertEqual(constants["max_in_flight"], 4)
        self.assertEqual(constants["flush_observation_count"], 1000)

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

    def test_committed_baseline_runs_satisfy_the_record_contract(self):
        runs = sorted(BASELINE_DIR.glob("*.json"))
        self.assertTrue(runs, "no committed baseline runs")
        for path in runs:
            with self.subTest(run=path.name):
                record = json.loads(path.read_text(encoding="utf-8"))
                self.assertEqual(harness.validate_run(record), [])


if __name__ == "__main__":
    unittest.main()
