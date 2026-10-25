#!/usr/bin/env python3
"""Intent checks for bounded synthetic-run lifecycle and safe diagnostics."""

import json
import importlib.util
import os
import signal
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

REPO = Path(__file__).resolve().parents[1]
RUNNER = REPO / "scripts/chaos-e2e-bounded.py"


class BoundedTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name) / "work"

    def start(self, body, mode="local", **limits):
        env = dict(os.environ, CHAOS_WORKDIR=str(self.work), CHAOS_MAX_SECONDS="8",
                   CHAOS_MAX_MIB="8", CHAOS_MAX_PROCESSES="32", CHAOS_BUNDLE_MIB="1")
        env.pop("CHAOS_SEED", None)
        env.pop("CHAOS_LANE", None)
        env.update(limits)
        return subprocess.Popen([sys.executable, str(RUNNER), mode, "--",
                                 sys.executable, "-c", body], env=env,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    def finish(self, child, expected):
        out, err = child.communicate(timeout=20)
        self.assertEqual(child.returncode, expected, (out, err))
        return json.loads((self.work / "bundle/runner.json").read_text())

    def test_default_workdir_uses_pinned_repo_storage(self):
        env = dict(os.environ, TMPDIR=self.temp.name, CHAOS_MAX_SECONDS="8",
                   CHAOS_MAX_MIB="8", CHAOS_MAX_PROCESSES="32", CHAOS_BUNDLE_MIB="1")
        for name in ("CHAOS_WORKDIR", "CHAOS_SEED", "CHAOS_LANE"):
            env.pop(name, None)
        body = """
import json,os,sys
from pathlib import Path
w=Path(os.environ['CHAOS_WORKDIR'])
print(json.dumps({'workdir':str(w)}),flush=True)
r=w/'run';r.mkdir();(r/'replay.json').write_text('{"resolved_seed":248,"applied_events":0}')
assert w.parent == Path(sys.argv[1]).resolve(), 'heavy fixture DBs must stay on pinned storage'
"""
        child = subprocess.run([sys.executable, str(RUNNER), "heavy", "--", sys.executable,
                                "-c", body, str(REPO / ".test-tmp")], env=env,
                               capture_output=True, timeout=20)
        line = next(line for line in child.stderr.decode().splitlines()
                    if line.startswith('{"workdir":'))
        located = Path(json.loads(line)["workdir"])
        try:
            self.assertEqual(child.returncode, 0, child.stderr.decode())
            self.assertEqual(located.parent, REPO / ".test-tmp")
        finally:
            if located.parent == REPO / ".test-tmp" and located.name.startswith("voom-chaos-"):
                shutil.rmtree(located)

    def test_large_diagnostic_index_stays_within_reserved_bytes(self):
        spec = importlib.util.spec_from_file_location("bounded", RUNNER)
        bounded = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(bounded)
        self.work.mkdir()
        receipt = {"files": [{"path": "x" * 1000} for _ in range(300)],
                   "omitted": [], "runs": [], "visualizations": []}
        bounded.write_index(self.work, receipt, 65536)
        self.assertLessEqual((self.work / "runner.json").stat().st_size, 65536)
        self.assertTrue(receipt["index_truncated"])

    def test_heavy_success_without_replay_fails_and_preserves_raw_evidence(self):
        body = "import os; from pathlib import Path; (Path(os.environ['CHAOS_EVIDENCE_ROOT'])/'owned-media').write_text('synthetic')"
        receipt = self.finish(self.start(body, mode="heavy"), 125)
        self.assertTrue((self.work / "evidence/owned-media").exists())
        self.assertTrue(receipt["raw_retained"])
        self.assertEqual(receipt["command_exit_code"], 0)
        self.assertEqual(receipt["reason"], "missing-replay-evidence")

    def test_cleanup_kills_even_when_process_accounting_fails(self):
        spec = importlib.util.spec_from_file_location("bounded", RUNNER)
        bounded = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(bounded)
        self.work.mkdir()
        ready = self.work / "ready"
        body = "import signal,sys,time;from pathlib import Path;signal.signal(signal.SIGTERM,signal.SIG_IGN);Path(sys.argv[1]).touch();time.sleep(60)"
        child = subprocess.Popen([sys.executable, "-c", body, str(ready)], start_new_session=True)
        try:
            deadline = time.monotonic() + 5
            while not ready.exists():
                self.assertLess(time.monotonic(), deadline)
                time.sleep(.01)
            with patch.object(bounded, "members", side_effect=RuntimeError("accounting failed")):
                with self.assertRaises(RuntimeError):
                    bounded.stop(child)
            self.assertIsNotNone(child.poll(), "inspection failure must not bypass KILL")
        finally:
            os.killpg(child.pid, signal.SIGKILL) if child.poll() is None else None
            child.wait(timeout=3)

    def test_failure_retains_replay_but_excludes_private_and_linked_inputs(self):
        body = '''
import json, os
from pathlib import Path
w = Path(os.environ['CHAOS_WORKDIR'])
r = w / 'run'; r.mkdir()
(r / 'replay.json').write_text(json.dumps({'resolved_seed':248,'applied_events':11}))
(r / 'scenario.yaml').write_text('seed: 248')
(r / 'manifest.current.json').write_text('{}')
(r / 'journal.jsonl').write_text('{}\\n')
(w / 'voom.db').write_text('PRIVATE_DB')
(w / 'node-agent.toml').write_text('PRIVATE_CONFIG')
(w / 'node-token').write_text('voom-node-v1.NEVER_RETAIN')
(w / 'stderr.log').write_text('Authorization: Bearer SECRET')
(w / 'linked').symlink_to(w.parent, target_is_directory=True)
(r / 'observed-state.json').symlink_to(w / 'node-token')
print('synthetic process receipt')
raise SystemExit(17)
'''
        receipt = self.finish(self.start(body), 17)
        bundle = self.work / "bundle"
        self.assertTrue((bundle / "run/replay.json").is_file())
        self.assertEqual(receipt["runs"][0]["applied_events"], 11)
        self.assertIsNone(receipt["requested_profile"], "ordinary runs are not fuzz generation")
        contents = b"".join(p.read_bytes() for p in bundle.rglob("*") if p.is_file())
        for secret in (b"PRIVATE_DB", b"PRIVATE_CONFIG", b"NEVER_RETAIN", b"Bearer SECRET"):
            self.assertNotIn(secret, contents)
        self.assertTrue(receipt["omitted"])
        self.assertLessEqual(sum(p.stat().st_size for p in bundle.rglob("*") if p.is_file()),
                             1024 * 1024)
        self.assertNotEqual(receipt["visualizations"][0]["exit_code"], 0)

    def test_special_files_and_linked_parents_cannot_block_or_escape_collection(self):
        body = """
import os
from pathlib import Path
w=Path(os.environ['CHAOS_WORKDIR'])
os.mkfifo(w/'journal.jsonl')
(w/'scenario.yaml').write_text('synthetic')
os.link(w/'scenario.yaml', w/'replay.json')
(w/'reports').symlink_to(w.parent, target_is_directory=True)
raise SystemExit(18)
"""
        receipt = self.finish(self.start(body), 18)
        self.assertFalse((self.work / "bundle/journal.jsonl").exists())
        self.assertFalse((self.work / "bundle/replay.json").exists())
        self.assertTrue(any(item["reason"] == "ValueError" for item in receipt["omitted"]))

    def test_oversized_diagnostic_is_omitted_without_losing_original_failure(self):
        body = "import os; from pathlib import Path; p=Path(os.environ['CHAOS_WORKDIR']); (p/'scenario.yaml').write_bytes(b'x'*2*1024*1024); raise SystemExit(19)"
        receipt = self.finish(self.start(body), 19)
        self.assertFalse((self.work / "bundle/scenario.yaml").exists())
        self.assertTrue(any(item["reason"] == "bundle-limit" for item in receipt["omitted"]))

    def test_descendants_are_dead_on_success_failure_timeout_and_interrupt(self):
        for mode, expected in (("success", 0), ("failure", 23), ("timeout", 124),
                               ("interrupt", 130), ("terminate", 143)):
            with self.subTest(mode=mode):
                self.work = Path(self.temp.name) / mode
                body = '''
import os, subprocess, sys, time
from pathlib import Path
w = Path(os.environ['CHAOS_WORKDIR'])
code = "import os,signal,time; from pathlib import Path; signal.signal(signal.SIGTERM,signal.SIG_IGN); Path(os.environ['CHAOS_WORKDIR'],'descendant.pid').write_text(str(os.getpid())); time.sleep(60)"
subprocess.Popen([sys.executable, '-c', code])
while not (w/'descendant.pid').exists(): time.sleep(.01)
'''
                body += {"success": "", "failure": "raise SystemExit(23)",
                         "timeout": "time.sleep(60)", "interrupt": "time.sleep(60)",
                         "terminate": "time.sleep(60)"}[mode]
                child = self.start(body, CHAOS_MAX_SECONDS="1" if mode == "timeout" else "8")
                if mode in ("interrupt", "terminate"):
                    deadline = time.monotonic() + 5
                    while not (self.work / "descendant.pid").exists():
                        self.assertLess(time.monotonic(), deadline)
                        time.sleep(.02)
                    child.send_signal(signal.SIGINT if mode == "interrupt" else signal.SIGTERM)
                self.finish(child, expected)
                pid = (self.work / "descendant.pid").read_text()
                state = subprocess.run(["ps", "-p", pid, "-o", "stat="],
                                       capture_output=True, text=True, check=False).stdout.strip()
                self.assertTrue(not state or state.startswith("Z"), (pid, state))

    def test_disk_and_process_thresholds_abort(self):
        disk = "import os,time; from pathlib import Path; (Path(os.environ['CHAOS_WORKDIR'])/'media.bin').write_bytes(b'x'*2*1024*1024); time.sleep(60)"
        self.assertEqual(self.finish(self.start(disk, CHAOS_MAX_MIB="1"), 125)["reason"],
                         "disk-limit")
        self.work = Path(self.temp.name) / "processes"
        processes = "import subprocess,sys,time; subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)']); time.sleep(60)"
        self.assertEqual(self.finish(self.start(processes, CHAOS_MAX_PROCESSES="1"), 125)["reason"],
                         "process-limit")

    def test_invalid_limits_and_nonempty_workdir_fail_before_execution(self):
        child = self.start("raise SystemExit(99)", CHAOS_MAX_SECONDS="nan")
        child.communicate(timeout=5)
        self.assertEqual(child.returncode, 2)
        self.work.mkdir(exist_ok=True)
        marker = self.work / "untouched"
        marker.write_text("private")
        child = self.start("raise SystemExit(99)")
        child.communicate(timeout=5)
        self.assertEqual(child.returncode, 2)
        self.assertEqual(marker.read_text(), "private")

    def test_workflow_only_publishes_allowlisted_failure_bundle(self):
        workflow = (REPO / ".github/workflows/chaos-e2e.yml").read_text()
        self.assertIn("workflow_dispatch:", workflow)
        self.assertIn('cron: "0 6 * * 1"', workflow)
        self.assertIn("id: upload-chaos-diagnostics", workflow)
        upload = workflow.split("id: upload-chaos-diagnostics", 1)[1].split("      - name:", 1)[0]
        self.assertIn("if: failure()", upload)
        self.assertIn("/chaos-e2e/bundle", upload)
        self.assertIn("retention-days: 7", upload)
        self.assertIn("chaos-e2e-diagnostics-${{ github.run_attempt }}", upload,
                      "rerun attempts must not collide with immutable artifacts")
        self.assertIn("author:app/github-actions", workflow)
        self.assertNotIn("chaos-e2e-ci", (REPO / "justfile").read_text().split("ci:", 1)[0])


if __name__ == "__main__":
    unittest.main()
