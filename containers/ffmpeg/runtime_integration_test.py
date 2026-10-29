#!/usr/bin/env python3
"""Exercise real operation cleanup, streams, mounts and ownership on one engine."""
import argparse
import json
import os
from pathlib import Path
import select
import signal
import shutil
import subprocess
import sys
import tempfile
import time
import uuid

import runtime


def wait_absent(engine, identity):
    deadline = time.monotonic() + 11
    while time.monotonic() < deadline:
        remaining = runtime.capture(engine.prefix + ["ps", "-a", "--no-trunc", "--filter",
                                    "id=" + identity, "--format", "{{.ID}}"])
        if not remaining.strip():
            return
        select.select([], [], [], 0.05)
    raise AssertionError("operation container survived cancellation: " + identity)


def launch(script, env, cwd, parent=False):
    argv = [sys.executable, str(Path(runtime.__file__).resolve()), "setup-tool", "/bin/sh",
            "-c", script]
    if parent:
        code = ("import os,subprocess,sys; "
                "p=subprocess.Popen(sys.argv[1:],env={**os.environ,"
                "'VOOM_MEDIA_CALLER_PID':str(os.getpid())}); p.wait()")
        argv = [sys.executable, "-c", code] + argv
    return subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            start_new_session=True)


def finish(process, data=b"", expected=0):
    try:
        stdout, stderr = process.communicate(data, timeout=20)
    except BaseException:
        process.kill()
        process.communicate(timeout=15)
        raise
    assert process.returncode == expected, (process.returncode, stderr.decode(errors="replace"))
    return stdout, stderr


def owned_ids(engine):
    return set(runtime.capture(engine.prefix + ["ps", "-a", "--no-trunc", "--filter",
                       "label=" + runtime.LABEL, "--format", "{{.ID}}"])
               .decode().splitlines())


def exercise(args, root):
    source, stage = root / "source with spaces", root / "staging with spaces"
    source.mkdir()
    stage.mkdir()
    witness = source / "witness"
    witness.write_bytes(b"explicit owned source witness\x00\xff")
    value = {"engine": args.engine, "image": args.image,
             "read_only_roots": [str(source)], "read_write_roots": [str(stage)]}
    if args.desktop:
        value["desktop"] = args.desktop
    config_path = root / "configuration.json"
    config_path.write_text(json.dumps(value))
    config = runtime.load_config(str(config_path), cwd=str(stage))
    engine = runtime.Engine(config)
    env = {**os.environ, "VOOM_MEDIA_CONTAINER_CONFIG": str(config_path),
           "VOOM_MEDIA_CALLER_PID": str(os.getpid())}
    name = "voom-runtime-sentinel-" + uuid.uuid4().hex
    sentinel = runtime.capture(engine.prefix + ["create", "--name", name, "--pull=never",
                                "--entrypoint", "/bin/sh", engine.image, "-c", "exit 0"])
    sentinel = sentinel.decode().strip()
    baseline = owned_ids(engine)
    results = []
    started = time.monotonic()
    try:
        setup = subprocess.run([sys.executable, runtime.__file__, "check-config",
                                "--source-witness", str(witness)], cwd=stage, env=env,
                               capture_output=True, timeout=30)
        assert setup.returncode == 0, setup.stderr.decode()
        results.append("source-witness-and-host-ownership")
        for tool in ("ffmpeg", "ffprobe"):
            result = subprocess.run([str(Path(runtime.__file__).with_name(tool + "-container")),
                                     "-version"], cwd=stage, env=env, capture_output=True, timeout=20)
            assert result.returncode == 0, result.stderr.decode()
            assert result.stdout.startswith((tool + " version 9.0.2").encode())
            results.append(tool)
        for script, data, expected in [("cat", bytes(range(256)) * 1024, 0),
                                        ("cat", b"", 0), ("exit 23", b"", 23)]:
            stdout, _ = finish(launch(script, env, stage), data, expected)
            if script == "cat":
                assert stdout == data
            results.append("binary-stdin" if data else "stdin-eof-or-exit")
        stdout, stderr = finish(launch("head -c 262144 /dev/zero; head -c 262144 /dev/zero >&2",
                                      env, stage))
        assert stdout == stderr == bytes(262144)
        results.append("concurrent-output-draining")
        # Source roots remain read-only; relative paths retain the configured cwd.
        script = 'if echo changed > "$1" 2>/dev/null; then exit 91; fi; echo output > relative'
        process = launch(script.replace('"$1"', "'" + str(witness).replace("'", "'\\''") + "'"),
                         env, stage)
        finish(process)
        assert witness.read_bytes() == b"explicit owned source witness\x00\xff"
        assert (stage / "relative").read_bytes() == b"output\n"
        assert (stage / "relative").stat().st_uid == os.getuid()
        results.append("readonly-relative-and-space-paths")
        # Interpose only on the test engine CLI: hold a successful create response
        # until launcher death, then prove the guardian reconciles its exact container.
        intercept = root / "engine-intercept"
        intercept.mkdir()
        marker, release = root / "created-id", root / "release-create"
        real_engine = shutil.which(args.engine)
        wrapper = intercept / args.engine
        wrapper.write_text("#!" + sys.executable + "\n" +
            "import pathlib,subprocess,sys,time\n" +
            "r=subprocess.run([" + repr(real_engine) + "]+sys.argv[1:],capture_output=True)\n" +
            "if 'create' in sys.argv and r.returncode == 0:\n" +
            " pathlib.Path(" + repr(str(marker)) + ").write_bytes(r.stdout)\n" +
            " deadline=time.monotonic()+15\n" +
            " while not pathlib.Path(" + repr(str(release)) + ").exists():\n" +
            "  if time.monotonic()>deadline: sys.exit(124)\n" +
            "  time.sleep(0.02)\n" +
            "sys.stdout.buffer.write(r.stdout);sys.stderr.buffer.write(r.stderr);sys.exit(r.returncode)\n")
        wrapper.chmod(0o755)
        process = launch("echo ready; sleep 60", {**env, "PATH": str(intercept) + os.pathsep + env["PATH"]}, stage)
        try:
            deadline = time.monotonic() + 10
            while not marker.exists() and time.monotonic() < deadline:
                select.select([], [], [], 0.02)
            assert marker.exists(), "engine did not create the barrier container"
            identity = marker.read_text().strip()
            process.kill()
            release.touch()
            process.communicate(timeout=15)
            wait_absent(engine, identity)
            results.append("launcher-kill-during-create-response")
        finally:
            release.touch()
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=15)
        for parent in (False, True):
            for sig in (signal.SIGTERM, signal.SIGKILL, None):
                process = launch("echo ready; sleep 60", env, stage, parent)
                try:
                    assert select.select([process.stdout], [], [], 10)[0], "operation did not start"
                    assert process.stdout.readline() == b"ready\n"
                    identities = owned_ids(engine) - baseline
                    assert len(identities) == 1, identities
                    before = time.monotonic()
                    if sig is None:
                        try:
                            process.communicate(timeout=0.01)
                            raise AssertionError("running operation did not exercise timeout")
                        except subprocess.TimeoutExpired:
                            process.kill()
                    else:
                        process.send_signal(sig)
                    process.communicate(timeout=15)
                    wait_absent(engine, identities.pop())
                    results.append({"cancellation": "caller" if parent else "launcher",
                                    "signal": sig.name if sig else "timeout", "seconds": round(time.monotonic() - before, 3)})
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.communicate(timeout=15)
                assert runtime.json_capture(engine.prefix + ["inspect", sentinel])[0]["Id"] == sentinel
        assert owned_ids(engine) == baseline, "operation cleanup left containers"
        return {"engine": args.engine, "platform": sys.platform, "cases": results,
                "seconds": round(time.monotonic() - started, 3)}
    finally:
        engine.remove(sentinel, time.monotonic() + 5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", choices=("docker", "podman"), required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--desktop")
    parser.add_argument("--root", type=Path,
                        default=Path(__file__).resolve().parents[2] / ".test-tmp")
    args = parser.parse_args()
    args.root.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="container-runtime-", dir=args.root.resolve()) as name:
        print(json.dumps(exercise(args, Path(name))))


if __name__ == "__main__":
    main()
