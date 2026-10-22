#!/usr/bin/env python3
"""Exercise acceptance setup at its subprocess and filesystem boundaries."""

import importlib.util
import itertools
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "acceptance_bootstrap", Path(__file__).with_name("acceptance-bootstrap.py")
)
bootstrap = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = bootstrap
spec.loader.exec_module(bootstrap)

TOKEN = "voom-node-v1.selftest-secret-never-retain"


class BootstrapTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.workdir = Path(self.temp.name) / "evidence"
        self.args = bootstrap.parser().parse_args(
            [
                "serve",
                "--workdir",
                str(self.workdir),
                "--input",
                str(Path(self.temp.name) / "input"),
            ]
        )
        self.stack = bootstrap.Bootstrap(self.args)
        self.addCleanup(self.stack.close)

    def assert_secret_absent(self, directory=None):
        for path in (directory or self.workdir).rglob("*"):
            if path.is_file():
                self.assertNotIn(TOKEN.encode(), path.read_bytes(), str(path))

    def test_registration_evidence_contains_only_nonsecret_node_metadata(self):
        envelope = {
            "schema_version": "0",
            "command": "node",
            "status": "ok",
            "data": {
                "node": {"id": 1, "name": "fixture", "kind": "remote"},
                "token": TOKEN,
                "token_hint": TOKEN[-12:],
            },
        }
        completed = subprocess.CompletedProcess([], 0, json.dumps(envelope), "")
        with patch.object(bootstrap.subprocess, "run", return_value=completed):
            result = self.stack.voom("node", "register", "--name", "fixture", "--kind", "remote")
        self.assertEqual(result["data"]["token"], TOKEN)
        evidence = list(self.workdir.rglob("*.json"))
        self.assertTrue(evidence)
        self.assert_secret_absent()
        self.assertTrue(all("token_hint" not in path.read_text() for path in evidence))

    def test_failed_registration_does_not_retain_raw_output_or_print_token(self):
        completed = subprocess.CompletedProcess([], 2, TOKEN, TOKEN)
        with (
            patch.object(bootstrap.subprocess, "run", return_value=completed),
            self.assertRaises(RuntimeError) as caught,
        ):
            self.stack.voom("node", "register", "--name", "fixture", "--kind", "remote")
        self.assertNotIn(TOKEN, str(caught.exception))
        self.assert_secret_absent()

    def test_malformed_node_metadata_cannot_smuggle_token_into_allowlisted_evidence(self):
        envelope = {"status": "ok", "data": {"node": {"id": TOKEN}, "token": TOKEN}}
        completed = subprocess.CompletedProcess([], 0, json.dumps(envelope), "")
        with (
            patch.object(bootstrap.subprocess, "run", return_value=completed),
            self.assertRaises(RuntimeError),
        ):
            self.stack.voom("node", "register", "--name", "fixture", "--kind", "remote")
        self.assert_secret_absent()

    def test_credential_is_private_outside_evidence_and_removed_at_shutdown(self):
        credential = self.stack.write_credential(TOKEN)
        self.assertFalse(credential.is_relative_to(self.workdir))
        self.assertEqual(credential.stat().st_mode & 0o777, 0o600)
        self.assertEqual(credential.parent.stat().st_mode & 0o777, 0o700)
        self.assertEqual(credential.read_text(), TOKEN)
        self.stack.close()
        self.assertFalse(credential.exists())
        self.assert_secret_absent()

    def test_cancelled_setup_does_not_start_another_cli_process(self):
        self.stack.stopping = True
        completed = subprocess.CompletedProcess([], 0, '{"status":"ok","data":{}}', "")
        with (
            patch.object(bootstrap.subprocess, "run", return_value=completed) as command,
            self.assertRaises(InterruptedError),
        ):
            self.stack.voom("scan", "--root", "1")
        command.assert_not_called()

    def test_failed_agent_or_activation_timeout_prevents_scan_and_reaps_children(self):
        handlers = {
            kind: bootstrap.signal.getsignal(kind)
            for kind in [bootstrap.signal.SIGTERM, bootstrap.signal.SIGINT]
        }
        for ordinal, agent_exits in enumerate([True, False]):
            with self.subTest(agent_exits=agent_exits):
                workdir = Path(self.temp.name) / f"failure-{ordinal}"
                args = bootstrap.parser().parse_args(
                    [
                        "smoke",
                        "--workdir",
                        str(workdir),
                        "--startup-timeout",
                        "3",
                    ]
                )
                commands = []
                children = []
                next_root = itertools.count(1)

                def run(command, commands=commands, next_root=next_root, **_kwargs):
                    if Path(command[0]).name != "voom":
                        return subprocess.CompletedProcess(command, 0, "", "")
                    operation = command[3:]
                    commands.append(operation)
                    if operation[:1] == ["scan"]:
                        self.fail("scan requested before authenticated root activation")
                    data = {}
                    if operation[:2] == ["node", "register"]:
                        data = {"node": {"id": 1}, "token": TOKEN}
                    elif operation[:2] == ["library", "add"]:
                        data = {"library_id": 1}
                    elif operation[:3] == ["library", "root", "add"]:
                        data = {"root_id": next(next_root)}
                    elif operation[:3] == ["library", "root", "list"]:
                        data = {"roots": []}
                    elif operation[:2] == ["worker", "list"]:
                        data = {"workers": []}
                    return subprocess.CompletedProcess(
                        command, 0, json.dumps({"status": "ok", "data": data}), ""
                    )

                class Child:
                    def __init__(
                        self, command, agent_exits=agent_exits, children=children, **kwargs
                    ):
                        self.returncode = None
                        if Path(command[0]).name == "voom-api":
                            kwargs["stderr"].write(
                                json.dumps(
                                    {"fields": {"event": "listening", "bind": "127.0.0.1:32123"}}
                                )
                                + "\n"
                            )
                            kwargs["stderr"].flush()
                        elif agent_exits:
                            self.returncode = 2
                        children.append(self)

                    def poll(self):
                        return self.returncode

                    def terminate(self):
                        self.returncode = 0

                    def kill(self):
                        self.returncode = -9

                    def wait(self, **_kwargs):
                        return self.returncode

                clock = [0.0]

                def advance(delay, clock=clock):
                    clock[0] += delay

                with (
                    patch.object(bootstrap.subprocess, "run", side_effect=run),
                    patch.object(bootstrap.subprocess, "Popen", Child),
                    patch.object(
                        bootstrap.shutil, "which", side_effect=lambda name: f"/fixture/{name}"
                    ),
                    patch.object(
                        bootstrap.time, "monotonic", side_effect=lambda clock=clock: clock[0]
                    ),
                    patch.object(bootstrap.time, "sleep", side_effect=advance),
                    self.assertRaisesRegex(RuntimeError, "activation"),
                ):
                    bootstrap.smoke(args)
                self.assertFalse(any(command[0] == "scan" for command in commands))
                self.assertEqual(len(children), 2)
                self.assertTrue(all(child.poll() is not None for child in children))
                shutdown = json.loads(
                    (workdir / "bootstrap-evidence" / "shutdown.json").read_text()
                )
                self.assertTrue(shutdown["credential_removed"])
                self.assertFalse(list(workdir.parent.glob(".voom-acceptance-secret-*")))
                self.assert_secret_absent(workdir)
                for kind, handler in handlers.items():
                    self.assertIs(bootstrap.signal.getsignal(kind), handler)


if __name__ == "__main__":
    unittest.main()
