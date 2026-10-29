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
        env = {**os.environ, "VOOM_MEDIA_CONTAINER_CONFIG": str(config),
               "VOOM_FFMPEG_BIN": str(tools / "ffmpeg-container"),
               "VOOM_FFPROBE_BIN": str(tools / "ffprobe-container"),
               "VOOM_MEDIA_CALLER_PID": str(os.getpid())}
        subprocess.run(["python3", str(tools / "runtime.py"), "check-config",
                        "--source-witness", str(witness)], cwd=stage, env=env, check=True)
        build = subprocess.run(["cargo", "test", "--manifest-path", str(repository / "Cargo.toml"),
                                "-p", "voom-ffmpeg-worker", "--test", "transcode_conformance",
                                "--no-run", "--message-format=json"],
                               cwd=repository, env=env, stdout=subprocess.PIPE, check=True)
        artifacts = [json.loads(line) for line in build.stdout.splitlines()]
        executables = [item["executable"] for item in artifacts
                       if item.get("reason") == "compiler-artifact"
                       and item["target"]["name"] == "transcode_conformance"
                       and item.get("executable")]
        if len(executables) != 1:
            raise RuntimeError("cargo did not identify exactly one conformance test executable")
        # Cargo runs tests in the crate directory. Invoke its exact artifact from the
        # granted stage instead, with an explicit owned temp root for generated media.
        subprocess.run([executables[0], args.filter, "--ignored", "--test-threads=1",
                        "--nocapture"], cwd=stage, env={**env, "TMPDIR": str(stage)}, check=True)



if __name__ == "__main__":
    main()
