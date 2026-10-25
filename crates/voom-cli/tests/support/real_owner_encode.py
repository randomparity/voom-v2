"""Run one chaos encode using the existing authenticated acceptance bootstrap."""

import importlib.util
import json
import os
import shutil
import subprocess
from pathlib import Path


def main():
    os.umask(0o077)
    repo = Path(__file__).resolve().parents[4]
    spec = importlib.util.spec_from_file_location(
        "bootstrap", repo / "scripts/acceptance-bootstrap.py"
    )
    bootstrap = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bootstrap)
    args = bootstrap.parser().parse_args()
    candidates = list(args.input[0].rglob("*.mp4"))
    if len(candidates) != 1:
        raise RuntimeError("chaos scenario must contain one MP4 candidate")
    # The scenario also has an MKV candidate; preserve this case's one-source scope.
    source_root = args.workdir.parent / "input"
    source_root.mkdir(mode=0o700)
    shutil.copyfile(candidates[0], source_root / candidates[0].name)
    args.input = [source_root]
    with bootstrap.Bootstrap(args) as stack:
        root = next(root for root in stack.roots if root["role"] == "input")
        scanned = stack.voom("scan", "--root", root["root_id"])["data"]
        if scanned.get("status") != "succeeded" or scanned.get("observation_count") != 1:
            raise RuntimeError("real encode requires exactly one successfully scanned source")
        policy = stack.voom(
            "policy", "create", "--slug", "video-transcode-hevc", "--file",
            repo / "crates/voom-policy/fixtures/policies/video-transcode-hevc.voom",
        )["data"]
        inputs = stack.voom(
            "policy", "input", "create-from-scan", "--root", root["root_id"],
            "--slug", "chaos-real-h264",
        )["data"]["input_set"]
        if inputs.get("included_count") != 1:
            raise RuntimeError("real encode requires exactly one policy input")
        selection = [
            "--policy-version-id", policy["version"]["version_id"],
            "--input-set-id", inputs["input_set_id"],
        ]
        plan = stack.voom("plan", "show", *selection)["data"]["plan"]
        stage = next(root for root in stack.roots if root["role"] == "staging")
        output = next(root for root in stack.roots if root["role"] == "output")
        execution = stack.voom(
            "compliance", "execute", *selection,
            "--staging-root", stage["provider_locator"],
            "--output-dir", output["provider_locator"],
        )["data"]
        produced = list(Path(output["provider_locator"]).rglob("*.mkv"))
        if len(produced) != 1 or not produced[0].is_file():
            raise RuntimeError("real encode must commit exactly one MKV output")
        probe = subprocess.run(
            ["ffprobe", "-v", "error", "-print_format", "json",
             "-show_format", "-show_streams", str(produced[0])],
            capture_output=True, text=True, timeout=30, check=True,
        )
        result = {"plan": plan, "execution": execution, "probe": json.loads(probe.stdout)}
    # Only emit success after Bootstrap has reaped its children and removed credentials.
    print(json.dumps(result), flush=True)


if __name__ == "__main__":
    main()
