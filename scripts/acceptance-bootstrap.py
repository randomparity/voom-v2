#!/usr/bin/env python3
"""Run a private local acceptance stack through authenticated owner activation."""

import argparse
import contextlib
import ipaddress
import json
import math
import os
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def seconds(value):
    parsed = float(value)
    if not math.isfinite(parsed) or not 0 < parsed <= 900:
        raise argparse.ArgumentTypeError("startup timeout must be within (0, 900] seconds")
    return parsed


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("mode", choices=["serve", "smoke"])
    result.add_argument("--workdir", type=Path, required=True)
    result.add_argument("--input", type=Path, action="append", default=[])
    result.add_argument("--staging", type=Path)
    result.add_argument("--output", type=Path)
    default_target = Path(
        os.environ.get("CARGO_TARGET_DIR", Path(__file__).resolve().parents[1] / "target")
    )
    result.add_argument("--bin-dir", type=Path, default=default_target / "debug")
    result.add_argument("--accelerator-json", type=Path)
    result.add_argument("--ffmpeg-bin", type=Path)
    result.add_argument("--startup-timeout", type=seconds, default=500.0)
    return result


def private_json(path, value):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as output:
        json.dump(value, output, indent=2)
        output.write("\n")


class Bootstrap:
    def __init__(self, args):
        self.args = args
        self.workdir = args.workdir.resolve()
        self.workdir.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.evidence = self.workdir / "bootstrap-evidence"
        self.evidence.mkdir(mode=0o700)
        self.url = "sqlite://" + str(self.workdir / "voom.db")
        self.bin_dir = args.bin_dir.resolve()
        self.env = dict(os.environ, VOOM_LOG_LEVEL="info")
        self.children = []
        self.credential_directory = None
        self.closed = False
        self.ready = False
        self.stopping = False
        self.startup_deadline = None
        self.signals = contextlib.ExitStack()
        self.counter = 0
        self.node_id = None
        self.library_id = None
        self.roots = []

    def __enter__(self):
        try:
            for kind in [signal.SIGTERM, signal.SIGINT]:
                previous = signal.signal(kind, self.request_stop)
                self.signals.callback(signal.signal, kind, previous)
            self.start()
        except BaseException:
            try:
                self.close()
            finally:
                self.signals.close()
            raise
        return self

    def __exit__(self, *_exception):
        try:
            self.close()
        finally:
            self.signals.close()

    def request_stop(self, _signal, _frame):
        self.stopping = True

    def voom(self, *arguments, record=True):
        self.require_alive()
        command = [str(self.bin_dir / "voom"), "--database-url", self.url, *map(str, arguments)]
        registration = arguments[:2] == ("node", "register")
        result = subprocess.run(
            command, env=self.env, capture_output=True, text=True, timeout=60, check=False
        )
        self.counter += 1
        try:
            envelope = json.loads(result.stdout)
        except (ValueError, TypeError):
            envelope = None
        valid = (
            isinstance(envelope, dict) and envelope.get("status") == "ok" and result.returncode == 0
        )
        if registration:
            # Registration is the only command whose successful envelope contains a bearer.
            data = envelope.get("data") if valid else None
            node = data.get("node") if isinstance(data, dict) else None
            node_id = node.get("id") if isinstance(node, dict) else None
            valid = (
                valid
                and type(node_id) is int
                and 0 < node_id < 2**64
                and isinstance(data.get("token"), str)
                and bool(data["token"])
            )
            safe = {
                "command": "node",
                "status": "ok" if valid else "error",
                "exit_code": result.returncode,
                "node_id": node_id if valid else None,
            }
        else:
            safe = (
                envelope
                if isinstance(envelope, dict)
                else {"status": "error", "exit_code": result.returncode}
            )
        if record:
            private_json(self.evidence / f"cli-{self.counter:04}.json", safe)
            if result.stderr and not registration:
                path = self.evidence / f"cli-{self.counter:04}.stderr"
                descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                with os.fdopen(descriptor, "w", encoding="utf-8") as output:
                    output.write(result.stderr)
        if not valid:
            operation = " ".join(map(str, arguments[:2]))
            raise RuntimeError(
                f"voom {operation} failed (exit {result.returncode}); inspect bootstrap evidence"
            )
        return envelope

    def write_credential(self, token):
        if not isinstance(token, str) or not token:
            raise RuntimeError("registration returned no usable node credential")
        self.credential_directory = tempfile.TemporaryDirectory(
            prefix=".voom-acceptance-secret-",
            dir=self.workdir if os.environ.get("VOOM_ACCEPTANCE_PROCESS_GROUP") == "inherit"
            else self.workdir.parent
        )
        path = Path(self.credential_directory.name) / "node-token"
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            output.write(token)
        return path

    def configure_roots(self):
        inputs = [path.resolve() for path in self.args.input]
        staging = (self.args.staging or self.workdir / "staging").resolve()
        output = (self.args.output or self.workdir / "output").resolve()
        paths = [*inputs, staging, output]
        if not inputs or len(inputs) > 62 or len(set(paths)) != len(paths):
            raise RuntimeError(
                "configure 1..62 distinct input directories and separate staging/output directories"
            )
        for index, first in enumerate(paths):
            for second in paths[index + 1 :]:
                if first.is_relative_to(second) or second.is_relative_to(first):
                    raise RuntimeError(
                        "input, staging and output roots must not contain one another"
                    )
        for path in inputs:
            if not path.is_dir():
                raise RuntimeError("an input root is not an existing directory")
        staging.mkdir(parents=True, exist_ok=True, mode=0o700)
        output.mkdir(parents=True, exist_ok=True, mode=0o700)
        for role, path in [
            ("staging", staging),
            ("output", output),
            *[("input", path) for path in inputs],
        ]:
            mode = "manual_recursive" if role == "input" else "explicit_only"
            data = self.voom(
                "library",
                "root",
                "add",
                "--library-id",
                self.library_id,
                "--owner-node-id",
                self.node_id,
                "--provider",
                "local_filesystem",
                "--provider-locator",
                path,
                "--scan-mode",
                mode,
            )["data"]
            self.roots.append(
                {"role": role, "provider_locator": str(path), "root_id": data["root_id"]}
            )
        stage_id = next(root["root_id"] for root in self.roots if root["role"] == "staging")
        output_id = next(root["root_id"] for root in self.roots if root["role"] == "output")
        for root in self.roots:
            self.voom(
                "library",
                "root",
                "update",
                "--root-id",
                root["root_id"],
                "--staging-root",
                stage_id,
                "--output-root",
                output_id,
            )

    def configuration(self, credential, base_url):
        quote = lambda value: json.dumps(str(value), ensure_ascii=False)
        lines = [
            f"control_plane_url = {quote(base_url)}",
            f"node_id = {self.node_id}",
            "poll_interval_ms = 100",
            "lease_ttl_seconds = 30",
            "progress_idle_timeout_seconds = 120",
            "shutdown_grace_seconds = 10",
            f'node_token = {{ source = "file", path = {quote(credential)} }}',
        ]
        for root in self.roots:
            lines += [
                "\n[[storage_roots]]",
                f"storage_root_id = {root['root_id']}",
                f"provider_locator = {quote(root['provider_locator'])}",
            ]
        accelerator = None
        if self.args.accelerator_json:
            document = self.args.accelerator_json.read_text().strip().removeprefix("BOUND ")
            accelerator = json.loads(document)
            accelerator = accelerator.get("accelerator", accelerator)
        tools = {}
        for name in ["ffmpeg", "ffprobe", "mkvmerge"]:
            selected = self.args.ffmpeg_bin if name == "ffmpeg" else None
            selected = selected or os.environ.get(f"VOOM_{name.upper()}_BIN") or shutil.which(name)
            if not selected:
                raise RuntimeError(
                    f"required media tool missing: {name}; install it on the owner host"
                )
            tools[name] = Path(selected).resolve()
        workers = [
            ("scan", "voom-scan-worker", ["scan_library"], []),
            ("hash", "voom-hash-worker", ["hash_file"], []),
            ("probe", "voom-ffprobe-worker", ["probe_file"], ["ffprobe"]),
            (
                "ffmpeg",
                "voom-ffmpeg-worker",
                ["transcode_video", "transcode_audio", "extract_audio"],
                ["ffmpeg", "ffprobe"],
            ),
            ("remux", "voom-mkvtoolnix-worker", ["remux"], ["mkvmerge"]),
            ("verify", "voom-verify-artifact-worker", ["verify_artifact"], []),
        ]
        for name, program, operations, dependencies in workers:
            capacity = accelerator["max_sessions"] if name == "ffmpeg" and accelerator else 2
            lines += [
                "\n[[workers]]",
                f"name = {quote(name)}",
                f"program = {quote(self.bin_dir / program)}",
                "args = []",
                f"operations = {json.dumps(operations)}",
                'artifact_access = ["shared_mount"]',
                f"max_parallel = {capacity}",
            ]
            if dependencies:
                lines += [
                    "[workers.dependencies]",
                    *[f"{tool}_bin = {quote(tools[tool])}" for tool in dependencies],
                ]
            if name == "ffmpeg" and accelerator:
                lines.append("[workers.accelerator]")
                decoder_tables = accelerator.get("decoders", [])
                if not decoder_tables or not isinstance(decoder_tables[0], dict):
                    decoder_tables = []
                for key, value in accelerator.items():
                    if key == "decoders" and decoder_tables:
                        continue
                    lines.append(f"{key} = {json.dumps(value, ensure_ascii=False)}")
                for decoder in decoder_tables:
                    lines.append("[[workers.accelerator.decoders]]")
                    for key, value in decoder.items():
                        lines.append(f"{key} = {json.dumps(value, ensure_ascii=False)}")
        return "\n".join(lines) + "\n"

    def launch(self, name, arguments):
        self.require_alive()
        with (
            open(self.evidence / f"{name}.stdout", "x", encoding="utf-8") as stdout,
            open(self.evidence / f"{name}.stderr", "x", encoding="utf-8") as stderr,
        ):
            process = subprocess.Popen(
                [str(self.bin_dir / name), *arguments],
                env=self.env,
                stdin=subprocess.DEVNULL,
                stdout=stdout,
                stderr=stderr,
                start_new_session=os.environ.get("VOOM_ACCEPTANCE_PROCESS_GROUP") != "inherit",
            )
        self.children.append((name, process))
        return process

    def require_alive(self):
        if self.stopping:
            raise InterruptedError("acceptance startup cancelled")
        if (
            not self.ready
            and self.startup_deadline is not None
            and time.monotonic() >= self.startup_deadline
        ):
            raise RuntimeError("timed out waiting for acceptance activation")
        for name, process in self.children:
            if process.poll() is not None:
                raise RuntimeError(
                    f"{name} exited during activation (exit {process.returncode}); inspect its bootstrap log"
                )

    def wait_api(self, deadline):
        log = self.evidence / "voom-api.stderr"
        while time.monotonic() < deadline:
            self.require_alive()
            for line in log.read_text().splitlines():
                try:
                    fields = json.loads(line).get("fields", {})
                except ValueError:
                    continue
                if fields.get("event") == "listening":
                    host, port = fields["bind"].rsplit(":", 1)
                    if not ipaddress.ip_address(host).is_loopback or not 0 < int(port) < 65536:
                        raise RuntimeError("API reported an invalid loopback listening address")
                    return f"http://{host}:{port}"
            time.sleep(0.1)
        raise RuntimeError("timed out waiting for API listening evidence")

    def wait_ready(self, deadline):
        while time.monotonic() < deadline:
            self.require_alive()
            roots = self.voom(
                "library", "root", "list", "--library-id", self.library_id, record=False
            )["data"]["roots"]
            workers = self.voom("worker", "list", "--status", "active", record=False)["data"][
                "workers"
            ]
            active_roots = {root["root_id"] for root in roots if root["effectively_available"]}
            active_workers = [
                worker["id"] for worker in workers if worker["node_id"] == self.node_id
            ]
            if (
                all(root["root_id"] in active_roots for root in self.roots)
                and len(active_workers) == 6
            ):
                self.ready = True
                receipt = {
                    "status": "ready",
                    "node_id": self.node_id,
                    "library_id": self.library_id,
                    "roots": self.roots,
                    "worker_ids": active_workers,
                }
                private_json(self.evidence / "ready.json", receipt)
                private_json(self.evidence / "active-roots.json", roots)
                private_json(self.evidence / "active-workers.json", workers)
                return receipt
            time.sleep(0.2)
        raise RuntimeError(
            "timed out waiting for authenticated root activation and six ready owner workers"
        )

    def start(self):
        deadline = time.monotonic() + self.args.startup_timeout
        self.startup_deadline = deadline
        if not (self.workdir / "voom.db").exists():
            self.voom("init")
        registered = self.voom(
            "node",
            "register",
            "--name",
            "acceptance-owner",
            "--kind",
            "remote",
            "--heartbeat-ttl-seconds",
            "60",
        )["data"]
        self.node_id = registered["node"]["id"]
        credential = self.write_credential(registered["token"])
        self.env["VOOM_LOCAL_NODE_ID"] = str(self.node_id)
        self.library_id = self.voom(
            "library",
            "add",
            "--slug",
            "acceptance",
            "--display-name",
            "Acceptance",
            "--media-kind",
            "movie",
        )["data"]["library_id"]
        self.configure_roots()
        self.launch(
            "voom-api",
            ["--database-url", self.url, "--bind", "127.0.0.1:0", "--allow-cleartext-loopback"],
        )
        base_url = self.wait_api(deadline)
        config_path = self.evidence / "node-agent.toml"
        descriptor = os.open(config_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as config:
            config.write(self.configuration(credential, base_url))
        self.launch("voom-node-agent", ["--config", str(config_path)])
        self.receipt = self.wait_ready(deadline)

    def close(self):
        if self.closed:
            return
        self.closed = True
        outcomes = []
        failures = []
        for name, process in reversed(self.children):
            try:
                if process.poll() is None:
                    process.terminate()
                try:
                    code = process.wait(timeout=45)
                except subprocess.TimeoutExpired:
                    process.kill()
                    code = process.wait(timeout=5)
                outcomes.append({"program": name, "exit_code": code})
                if self.ready and code != 0:
                    failures.append(name)
            except (OSError, subprocess.TimeoutExpired) as error:
                failures.append(f"{name}: {type(error).__name__}")
        if self.credential_directory is not None:
            self.credential_directory.cleanup()
        private_json(
            self.evidence / "shutdown.json", {"children": outcomes, "credential_removed": True}
        )
        if failures:
            raise RuntimeError("acceptance child shutdown failed: " + ", ".join(failures))


def smoke(args):
    args.workdir.mkdir(parents=True, exist_ok=True, mode=0o700)
    source = args.workdir / "input"
    source.mkdir(mode=0o700)
    ffmpeg = args.ffmpeg_bin or shutil.which("ffmpeg")
    if not ffmpeg:
        raise RuntimeError("smoke requires ffmpeg on the owner host")
    subprocess.run(
        [
            str(ffmpeg),
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=5",
            "-t",
            "1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            str(source / "source.mkv"),
        ],
        check=True,
    )
    args.input = [source]
    with Bootstrap(args) as stack:
        root = next(root for root in stack.roots if root["role"] == "input")
        scanned = stack.voom("scan", "--root", root["root_id"])
        if (
            scanned["data"].get("status") != "succeeded"
            or scanned["data"].get("observation_count") != 1
        ):
            raise RuntimeError("software smoke did not publish exactly one scanned file")
        result = stack.voom(
            "policy",
            "input",
            "create-from-scan",
            "--root",
            root["root_id"],
            "--slug",
            "acceptance-smoke",
        )
        if result["data"]["input_set"].get("included_count") != 1:
            raise RuntimeError("software smoke did not include exactly one policy input")
        print(
            json.dumps(
                {
                    "status": "ok",
                    "node_id": stack.node_id,
                    "root_id": root["root_id"],
                    "input_set_id": result["data"]["input_set"]["input_set_id"],
                }
            ),
            flush=True,
        )


def serve(args):
    with Bootstrap(args) as stack:
        print(json.dumps(stack.receipt), flush=True)
        while not stack.stopping:
            readable, _, _ = select.select([sys.stdin], [], [], 0.2)
            if readable and not os.read(sys.stdin.fileno(), 1):
                break
            if stack.stopping:
                break
            stack.require_alive()


def main():
    args = parser().parse_args()
    try:
        (smoke if args.mode == "smoke" else serve)(args)
    except (RuntimeError, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(f"acceptance bootstrap failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
