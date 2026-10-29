#!/usr/bin/env python3
"""Run the required PCM worker tests through the installed container launchers."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", choices=("docker", "podman"), required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--desktop")
    parser.add_argument("--filter", default="pcm_container_")
    parser.add_argument("--root", type=Path)
    args = parser.parse_args()
    repository = Path(__file__).resolve().parents[2]
    root = args.root or repository / ".test-tmp"
    root.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="pcm-worker-", dir=root.resolve()) as temporary:
        owned = Path(temporary)
        source, stage = owned / "source", owned / "stage"
        source.mkdir()
        stage.mkdir()
        witness = source / "witness"
        witness.write_text("generated PCM worker setup witness\n")
        value = {"engine": args.engine, "image": args.image,
                 "read_only_roots": [str(source)], "read_write_roots": [str(stage)]}
        if args.desktop:
            value["desktop"] = args.desktop
        config = owned / "config.json"
        config.write_text(json.dumps(value))
        tools = repository / "containers" / "ffmpeg"
        installed = subprocess.run(
            ["python3", str(tools / "install.py"), "--config", str(config),
             "--destination", str(owned / "installed"), "--source-witness", str(witness)],
            cwd=stage, stdout=subprocess.PIPE, check=True)
        paths = json.loads(installed.stdout)
        env = {**os.environ, "VOOM_FFMPEG_BIN": paths["ffmpeg_bin"],
               "VOOM_FFPROBE_BIN": paths["ffprobe_bin"], "VOOM_TEST_PREBUILT_WORKERS": "1"}
        env.pop("VOOM_MEDIA_CONTAINER_CONFIG", None)
        subprocess.run(["cargo", "build", "-p", "voom-ffprobe-worker",
                        "--bin", "voom-ffprobe-worker"], cwd=repository, env=env, check=True)
        targets = [("voom-ffmpeg-worker", "transcode_conformance", args.filter),
                   ("voom-node-agent", "container_audio", "installed_container_tools_")]
        commands = []
        for package, target, selection in targets:
            build = subprocess.run(
                ["cargo", "test", "-p", package, "--test", target,
                 "--no-run", "--message-format=json"], cwd=repository, env=env,
                stdout=subprocess.PIPE, check=True)
            artifacts = [json.loads(line) for line in build.stdout.splitlines()]
            executables = [item["executable"] for item in artifacts
                           if item.get("reason") == "compiler-artifact"
                           and item["target"]["name"] == target and item.get("executable")]
            if len(executables) != 1:
                raise RuntimeError(f"cargo did not identify exactly one {target} executable")
            listing = subprocess.run([executables[0], selection, "--ignored", "--list"],
                                     cwd=stage, env=env, capture_output=True, check=True)
            if b": test" not in listing.stdout:
                raise RuntimeError(f"required {target} selection matched no tests: {selection}")
            commands.append([executables[0], selection, "--ignored", "--test-threads=1",
                             "--nocapture"])
        # Finish every build before media runs. Cargo's crate cwd is outside the
        # grants, so invoke exact artifacts from the owned staging directory.
        for command in commands:
            subprocess.run(command, cwd=stage, env={**env, "TMPDIR": str(stage)}, check=True)


if __name__ == "__main__":
    main()
