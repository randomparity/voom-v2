"""Exercise production scan/hash/probe without supplying normalized evidence."""

import importlib.util
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import time
from pathlib import Path


def main():
    os.umask(0o077)
    repo = Path(__file__).resolve().parents[4]
    spec = importlib.util.spec_from_file_location(
        "bootstrap", repo / "scripts/acceptance-bootstrap.py"
    )
    bootstrap = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bootstrap)
    existing = "--existing-fixtures" in sys.argv
    argv = [arg for arg in sys.argv[1:] if arg != "--existing-fixtures"]
    args = bootstrap.parser().parse_args(argv)
    library = args.input[0]
    args.workdir.mkdir(mode=0o700)
    if not existing:
        subtitle = args.workdir / "fixture.srt"
        subtitle.write_text("1\n00:00:00,000 --> 00:00:00,400\nEvidence\n")
        subprocess.run(
            ["ffmpeg", "-v", "error", "-nostdin", "-f", "lavfi", "-i",
             "color=size=64x64:rate=5:duration=0.4", "-f", "lavfi", "-i",
             "sine=duration=0.4", "-i", str(subtitle), "-map", "0:v", "-map", "1:a",
             "-map", "2:s", "-c:v", "libx264", "-threads", "1", "-c:a", "aac",
             "-c:s", "srt", "-disposition:a:0", "default", "-disposition:s:0", "forced",
             "-metadata", "title=Persisted Évidence", "-metadata", "year=2026",
             str(library / "healthy.mkv")],
            capture_output=True, timeout=30, check=True,
        )
        (library / "invalid.mp4").write_bytes(b"not an MP4 container\n")
    # The shim observes actual worker tool calls, without implementing worker
    # requests or returning invented probe facts. Both streams remain inherited.
    trace = args.workdir / "probe-invocations.jsonl"
    shim = args.workdir / "ffprobe-receipt"
    real_ffprobe = shutil.which("ffprobe")
    if not real_ffprobe:
        raise RuntimeError("ffprobe is required")
    shim.write_text(
        "#!/usr/bin/env python3\n"
        "import json, os, subprocess, sys\n"
        f"result = subprocess.run([{real_ffprobe!r}, *sys.argv[1:]], timeout=30)\n"
        f"fd = os.open({str(trace)!r}, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)\n"
        "os.write(fd, (json.dumps({'args': sys.argv[1:], 'exit_code': result.returncode}) + '\\n').encode())\n"
        "os.close(fd)\n"
        "sys.exit(result.returncode)\n"
    )
    shim.chmod(0o700)
    os.environ["VOOM_FFPROBE_BIN"] = str(shim)
    with bootstrap.Bootstrap(args) as stack:
        root = next(root for root in stack.roots if root["role"] == "input")
        scanned = stack.voom("scan", "--root", root["root_id"])["data"]
        # Scan publication precedes ticket settlement. Wait for the durable
        # result, not a fixed sleep or the CLI publication response alone.
        with sqlite3.connect(stack.workdir / "voom.db") as db:
            deadline = time.monotonic() + 30
            while True:
                rows = db.execute(
                    "SELECT result FROM tickets WHERE kind = 'synthetic.workflow.operation.scan_library' AND state = 'succeeded'"
                ).fetchall()
                if len(rows) == 1 and rows[0][0] is not None:
                    completion = json.loads(rows[0][0])
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError("scan ticket did not settle after publication")
                time.sleep(0.05)
        result = {"scan": scanned, "completion": completion}
    result["probe_invocations"] = [json.loads(line) for line in trace.read_text().splitlines()]
    print(json.dumps(result), flush=True)


if __name__ == "__main__":
    main()
