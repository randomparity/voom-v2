#!/usr/bin/env python3
"""Opt-in, operation-owned Docker/Podman execution for the paired media tools."""
import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import select
import stat
import subprocess
import sys
import time
import uuid

DEADLINE = 5
INTERVAL = 0.05
LABEL = "io.voom.media-operation"


class SetupError(Exception):
    """An unmet operator prerequisite or failed operation cleanup."""


@dataclass(frozen=True)
class Config:
    engine: str
    image: str
    read_only_roots: tuple
    read_write_roots: tuple
    desktop: str = ""


def within(path, root):
    return path == root or path.startswith(root + os.sep)


def canonical(path):
    if not isinstance(path, str) or not os.path.isabs(path):
        raise SetupError("use an absolute canonical path")
    if any(character in path for character in ",\n\r\x00"):
        raise SetupError("mount paths cannot contain commas, newlines or NUL")
    if os.path.realpath(path) != path or path == "/" or not os.path.isdir(path):
        raise SetupError("mount roots must be existing canonical directories other than /")
    return path


def load_config(path, system=None, cwd=None):
    system = system or platform.system()
    if system not in ("Linux", "Darwin"):
        raise SetupError("container media tools require Linux or macOS")
    if not path or not os.path.isabs(path):
        raise SetupError("set VOOM_MEDIA_CONTAINER_CONFIG to an absolute JSON file")
    try:
        with open(path, encoding="utf-8") as source:
            value = json.load(source)
    except (OSError, ValueError) as error:
        raise SetupError(f"cannot read container configuration: {error}") from error
    required = {"engine", "image", "read_only_roots", "read_write_roots"}
    allowed = required | ({"desktop"} if system == "Darwin" else set())
    if not isinstance(value, dict) or set(value) != allowed:
        raise SetupError("configuration requires exactly engine, image, read_only_roots, "
                         "read_write_roots and, on macOS, desktop")
    if value["engine"] not in ("docker", "podman"):
        raise SetupError("engine must be docker or podman")
    if not isinstance(value["image"], str) or not re.fullmatch(
            r"(?:[a-zA-Z0-9._/:+-]+@)?sha256:[0-9a-f]{64}", value["image"]):
        raise SetupError("image must be an existing immutable sha256 image ID or repository digest")
    desktop = value.get("desktop", "")
    if system == "Darwin" and (not isinstance(desktop, str) or not desktop or
                              desktop.startswith("-") or "\n" in desktop):
        raise SetupError("desktop must name an existing Docker context or Podman machine")
    roots = []
    for field in ("read_only_roots", "read_write_roots"):
        if not isinstance(value[field], list) or not value[field]:
            raise SetupError(f"{field} must be a nonempty list of directory grants")
        roots.extend(canonical(root) for root in value[field])
    for index, root in enumerate(roots):
        if any(within(root, other) or within(other, root) for other in roots[:index]):
            raise SetupError("mount roots must be disjoint, without duplicates or nesting")
    cwd = cwd or os.getcwd()
    if os.path.realpath(cwd) != cwd or not any(within(cwd, root) for root in roots):
        raise SetupError("run the worker from a canonical directory within a configured root")
    return Config(value["engine"], value["image"], tuple(value["read_only_roots"]),
                  tuple(value["read_write_roots"]), desktop)


class CallerWatch:
    def __init__(self):
        self.fd = None
        self.queue = None
        expected = os.environ.get("VOOM_MEDIA_CALLER_PID", "")
        if not expected.isdecimal() or int(expected) <= 1 or int(expected) != os.getppid():
            raise SetupError("caller must set VOOM_MEDIA_CALLER_PID to its own PID before spawn")
        self.expected = int(expected)
        try:
            if platform.system() == "Linux":
                self.fd = os.pidfd_open(self.expected)
            elif platform.system() == "Darwin":
                self.queue = select.kqueue()
                event = select.kevent(self.expected, filter=select.KQ_FILTER_PROC,
                                     flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                                     fflags=select.KQ_NOTE_EXIT)
                events = self.queue.control([event], 1, 0)
                if events:
                    raise SetupError("caller exited or process-watch registration failed")
            else:
                raise SetupError("process exit monitoring requires Linux pidfd or macOS kqueue")
            if self.dead():
                raise SetupError("caller exited before container launch")
        except (OSError, AttributeError) as error:
            self.close()
            raise SetupError(f"cannot register caller exit watch: {error}") from error
        except SetupError:
            self.close()
            raise

    def dead(self):
        if os.getppid() != self.expected:
            return True
        if self.fd is not None:
            return bool(select.select([self.fd], [], [], 0)[0])
        return bool(self.queue.control(None, 1, 0))

    def close(self):
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None
        if self.queue is not None:
            self.queue.close()
            self.queue = None

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def capture(argv, timeout=DEADLINE, data=None):
    try:
        result = subprocess.run(argv, input=data, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, timeout=max(timeout, 0.01),
                                close_fds=True)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise SetupError(f"container command failed ({argv[0]}): {error}") from error
    if result.returncode:
        raise SetupError(f"container command {argv[1:3]} exited {result.returncode}: "
                         f"{result.stderr.decode(errors='replace').strip()}")
    return result.stdout


def json_capture(argv, timeout=DEADLINE):
    try:
        return json.loads(capture(argv, timeout))
    except (ValueError, TypeError) as error:
        raise SetupError("container engine returned malformed JSON") from error


def unix_endpoint(endpoint):
    if not isinstance(endpoint, str) or not endpoint.startswith("unix://"):
        raise SetupError("configure a local Unix-socket engine endpoint; remote TCP/SSH is unsupported")
    try:
        if not stat.S_ISSOCK(os.stat(endpoint[7:]).st_mode):
            raise SetupError("configured engine endpoint is not a Unix socket")
    except OSError as error:
        raise SetupError("start the configured local engine/Desktop machine") from error
    return endpoint


class Engine:
    def __init__(self, config):
        self.config = config
        for name in ("DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_TLS_VERIFY", "DOCKER_CERT_PATH",
                     "CONTAINER_HOST", "CONTAINER_CONNECTION"):
            if os.environ.get(name):
                raise SetupError(f"unset conflicting engine steering {name}; use the configuration")
        if config.engine == "docker":
            context = config.desktop or "default"
            records = json_capture(["docker", "context", "inspect", context])
            endpoint = unix_endpoint(records[0]["Endpoints"]["docker"]["Host"])
            self.prefix = ["docker", "--host", endpoint]
            info = json_capture(self.prefix + ["info", "--format", "{{json .}}"])
            if info.get("OSType") != "linux":
                raise SetupError("Docker must run Linux containers")
            security = " ".join(info.get("SecurityOptions", []))
            if "rootless" in security or "userns" in security:
                raise SetupError("Docker profile requires rootful Docker without userns-remap")
            if config.desktop and "docker desktop" not in info.get("OperatingSystem", "").lower():
                raise SetupError("macOS context must select a local Docker Desktop Linux engine")
            architecture = info["Architecture"]
        else:
            if config.desktop:
                record = json_capture(["podman", "machine", "inspect", config.desktop])[0]
                if record.get("Name") != config.desktop or record.get("State") != "running" or \
                        record.get("Rootful") is not False:
                    raise SetupError("start the named rootless Podman Desktop machine")
                endpoint = unix_endpoint("unix://" + record["ConnectionInfo"]["PodmanSocket"]["Path"])
                self.prefix = ["podman", "--url", endpoint]
            else:
                self.prefix = ["podman", "--remote=false"]
            info = json_capture(self.prefix + ["info", "--format", "json"])
            if not info["host"]["security"]["rootless"]:
                raise SetupError("Podman profile requires a rootless engine")
            architecture = info["host"]["arch"]
        architecture = {"x86_64": "amd64", "aarch64": "arm64"}.get(architecture, architecture)
        image = json_capture(self.prefix + ["image", "inspect", config.image])[0]
        if image.get("Os") != "linux" or image.get("Architecture") != architecture or \
                architecture not in ("amd64", "arm64"):
            raise SetupError("build the configured image for the engine's native amd64/arm64 platform")
        self.image = image["Id"]

    def create_args(self, name, executable, argv):
        args = ["create", "--name", name, "--label", LABEL + "=" + name,
                "--pull=never", "--network=none", "--read-only", "--cap-drop=ALL",
                "--security-opt=no-new-privileges", "-i", "--workdir", os.getcwd()]
        if self.config.engine == "docker":
            args += ["--user", f"{os.getuid()}:{os.getgid()}"]
        else:
            args += ["--userns=keep-id:uid=65532,gid=65532", "--user", "65532:65532"]
        for roots, readonly in ((self.config.read_only_roots, True),
                                (self.config.read_write_roots, False)):
            for root in roots:
                args += ["--mount", f"type=bind,source={root},target={root}" +
                         (",readonly" if readonly else "")]
        return args + ["--entrypoint", executable, self.image] + argv

    def remove(self, identity, deadline):
        args = ["rm", "-f"] + (["--time", "0"] if self.config.engine == "podman" else [])
        try:
            capture(self.prefix + args + [identity], deadline - time.monotonic())
        except SetupError:
            # Only a successful exact-ID inventory can prove absence after a failed rm.
            remaining = capture(self.prefix + ["ps", "-a", "--no-trunc", "--filter",
                                "id=" + identity, "--format", "{{.ID}}"],
                                deadline - time.monotonic())
            if remaining.strip():
                raise SetupError(f"cleanup failed; remove owned container {identity}")

    def reconcile(self, name, deadline):
        records = capture(self.prefix + ["ps", "-a", "--no-trunc", "--filter", "name=" + name,
                          "--filter", "label=" + LABEL + "=" + name, "--format", "{{.ID}}"],
                          deadline - time.monotonic()).decode().splitlines()
        for identity in records:
            record = json_capture(self.prefix + ["inspect", identity], deadline - time.monotonic())[0]
            if record["Name"].lstrip("/") == name and \
                    record["Config"]["Labels"].get(LABEL) == name:
                self.remove(identity, deadline)


def lease_closed(fd):
    return bool(select.select([fd], [], [], 0)[0])


def diagnostic(message):
    try:
        print("voom container: " + str(message), file=sys.stderr, flush=True)
    except (OSError, BrokenPipeError):
        pass


def guardian(config, executable, argv, lease):
    engine = None
    identity = None
    attachment = None
    name = "voom-media-" + uuid.uuid4().hex
    status = 125
    try:
        engine = Engine(config)
        if lease_closed(lease):
            return 125
        result = capture(engine.prefix + engine.create_args(name, executable, argv))
        identity = result.decode().strip()
        if not re.fullmatch(r"[0-9a-f]{64}", identity):
            identity = None
            raise SetupError("engine returned an invalid container ID")
        if not lease_closed(lease):
            attachment = subprocess.Popen(engine.prefix + ["start", "-a", "-i", identity],
                                          close_fds=True)
            while attachment.poll() is None and not lease_closed(lease):
                select.select([lease], [], [], INTERVAL)
            if attachment.returncode is not None:
                status = attachment.returncode if attachment.returncode >= 0 else 128 - attachment.returncode
    except (SetupError, OSError, KeyError, TypeError, IndexError) as error:
        diagnostic(error)
    finally:
        deadline = time.monotonic() + DEADLINE
        try:
            if engine is not None:
                if identity:
                    engine.remove(identity, deadline)
                else:
                    engine.reconcile(name, deadline)
        except (SetupError, OSError, KeyError, TypeError, IndexError) as error:
            diagnostic(f"cleanup unconfirmed for owned operation {identity or name}: {error}")
            status = 125
        if attachment is not None and attachment.poll() is None:
            attachment.terminate()
            try:
                attachment.wait(timeout=max(0.01, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                attachment.kill()
                attachment.wait()
        os.close(lease)
    return status


def run_operation(config, executable, argv):
    with CallerWatch() as watch:
        read_fd, write_fd = os.pipe()
        pid = os.fork()
        if pid == 0:
            os.close(write_fd)
            watch.close()
            os.setsid()
            os._exit(guardian(config, executable, argv, read_fd))
        os.close(read_fd)
        try:
            while True:
                child, status = os.waitpid(pid, os.WNOHANG)
                if child:
                    return os.waitstatus_to_exitcode(status)
                if watch.dead():
                    return 125
                select.select([], [], [], INTERVAL)
        finally:
            os.close(write_fd)


def check_config(config, witnesses):
    if len(witnesses) != len(config.read_only_roots):
        raise SetupError("provide exactly one source witness per read-only root")
    for root in config.read_only_roots:
        selected = [Path(path) for path in witnesses if within(path, root)]
        if len(selected) != 1 or not selected[0].is_file() or selected[0].stat().st_size == 0:
            raise SetupError("provide one existing nonempty --source-witness file per read-only root")
        path = selected[0]
        if str(path.resolve()) != str(path):
            raise SetupError("source witnesses must use canonical paths")
        digest = hashlib.sha256()
        with path.open("rb") as source:
            for block in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(block)
        expected = digest.hexdigest()
        result = setup_capture("/usr/bin/sha256sum", [str(path)])
        if result.decode().split()[0] != expected:
            raise SetupError("source witness differs in the VM; provision the same-path share")
    for root in config.read_write_roots:
        path = Path(root) / (".voom-container-check-" + uuid.uuid4().hex)
        nonce = os.urandom(32)
        try:
            path.write_bytes(nonce)
            result = setup_capture("/bin/sh", ["-c", 'cat "$1"; cat > "$2"',
                                   "voom-check", str(path), str(path) + ".out"], nonce)
            output = Path(str(path) + ".out")
            if result != nonce or output.read_bytes() != nonce or \
                    (output.stat().st_uid, output.stat().st_gid) != (os.getuid(), os.getgid()):
                raise SetupError("staging share failed content/host ownership proof; provision its mapping")
        finally:
            path.unlink(missing_ok=True)
            Path(str(path) + ".out").unlink(missing_ok=True)
    print("Container roots verified; source witnesses were read without modification.")


def setup_capture(executable, argv, data=None):
    # A child invocation preserves the same parent-death/guardian boundary as media tools.
    env = {**os.environ, "VOOM_MEDIA_CALLER_PID": str(os.getpid())}
    try:
        result = subprocess.run([sys.executable, __file__, "setup-tool", executable] + argv,
                                input=data, capture_output=True, env=env, timeout=20)
    except subprocess.TimeoutExpired as error:
        raise SetupError("container access check timed out; inspect the configured engine and shares") from error
    if result.returncode:
        raise SetupError("container access check failed; provision accessible same-path roots: " +
                         result.stderr.decode(errors="replace"))
    return result.stdout


def main():
    try:
        config = load_config(os.environ.get("VOOM_MEDIA_CONTAINER_CONFIG"))
        if len(sys.argv) < 2:
            raise SetupError("select ffmpeg, ffprobe or check-config")
        tool = sys.argv[1]
        if tool == "check-config":
            parser = argparse.ArgumentParser()
            parser.add_argument("--source-witness", action="append", default=[])
            args = parser.parse_args(sys.argv[2:])
            check_config(config, args.source_witness)
            return 0
        if tool == "setup-tool":
            if len(sys.argv) < 3 or sys.argv[2] not in ("/usr/bin/sha256sum", "/bin/sh"):
                raise SetupError("invalid setup tool")
            return run_operation(config, sys.argv[2], sys.argv[3:])
        if tool not in ("ffmpeg", "ffprobe"):
            raise SetupError("invalid media tool")
        if any(os.environ.get(name) for name in ("VOOM_NVIDIA_DEVICE", "VOOM_VAAPI_DEVICE",
                                                "VOOM_VIDEOTOOLBOX_RESOURCE_ID")):
            raise SetupError("container profile is CPU-only; unset the selected hardware device")
        return run_operation(config, "/usr/local/bin/" + tool, sys.argv[2:])
    except (SetupError, OSError, KeyError, TypeError, IndexError) as error:
        diagnostic(error)
        return 125


if __name__ == "__main__":
    sys.exit(main())
