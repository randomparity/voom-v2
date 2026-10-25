#!/usr/bin/env python3
"""Bound repository-owned synthetic chaos runs and publish selected diagnostics."""

import argparse
import hashlib
import json
import os
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
UPSTREAM = REPO / "third_party/chaos-librarian"
MIB = 1024 * 1024
SECRET = re.compile(rb"voom-node-v1\.[^\s\"'<>]+|(?i:bearer\s+[^\s\"'<>]+|(?:node_)?token[\"']?\s*[:=]\s*[\"']?[^\s,}\"']+|password[\"']?\s*[:=])")
NAMES = {"scenario.yaml", "replay.json", "manifest.initial.json", "manifest.current.json",
         "materialization.json", "validation.json", "journal.jsonl", "observed-state.json",
         "comparison.json", "timeline-summary.json", "summary.jsonl", "chaos-run.json",
         "generation.json", "command.stdout", "command.stderr", "chaos.stderr",
         "stdout.json", "stderr.log", "bootstrap.stderr"}


def integer(name, default, maximum):
    value = os.environ.get(name, str(default))
    if not re.fullmatch(r"[1-9][0-9]*", value) or int(value) > maximum:
        raise ValueError(f"{name} must be an integer within 1..{maximum}")
    return int(value)


def regular_files(root):
    for directory, dirs, files in os.walk(root, followlinks=False):
        dirs[:] = sorted(d for d in dirs if not (Path(directory) / d).is_symlink())
        for name in sorted(files):
            path = Path(directory) / name
            try:
                if stat.S_ISREG(path.lstat().st_mode):
                    yield path
            except FileNotFoundError:
                continue


def disk_bytes(root):
    total = 0
    for path in regular_files(root):
        try:
            total += path.stat().st_size
        except FileNotFoundError:
            continue
    return total


def members(group):
    result = subprocess.run(["ps", "-A", "-o", "pgid=,stat="], capture_output=True,
                            text=True, timeout=2, check=True)
    return sum(1 for line in result.stdout.splitlines()
               if len(parts := line.split()) >= 2 and parts[0] == str(group)
               and not parts[1].startswith("Z"))


def send_group(group, kind):
    try:
        os.killpg(group, kind)
    except ProcessLookupError:
        pass


def stop(child):
    send_group(child.pid, signal.SIGTERM)
    deadline = time.monotonic() + 2
    try:
        while members(child.pid) and time.monotonic() < deadline:
            child.poll()
            time.sleep(.05)
    finally:
        # Accounting failure must never bypass escalation for a TERM-ignoring child.
        send_group(child.pid, signal.SIGKILL)
        child.wait(timeout=3)
    deadline = time.monotonic() + 2
    while members(child.pid) and time.monotonic() < deadline:
        time.sleep(.05)
    if members(child.pid):
        raise RuntimeError("live descendants remained after KILL")


def execute(command, directory, limits, env, interrupted, *, cwd=None):
    started = time.monotonic()
    result = {"exit_code": 125, "reason": "runner-error"}
    with (directory / "command.stdout").open("wb") as out, \
         (directory / "command.stderr").open("wb") as err:
        child = subprocess.Popen(command, cwd=cwd, env=env, stdout=out, stderr=err,
                                 start_new_session=True)
        try:
            while True:
                code = child.poll()
                if code is not None and code != 0:
                    result = {"exit_code": code if code >= 0 else 128 - code,
                              "reason": "command-failed"}
                    break
                if interrupted[0]:
                    result = {"exit_code": interrupted[0], "reason": "interrupted"}
                    break
                if time.monotonic() - started >= limits["seconds"]:
                    result = {"exit_code": 124, "reason": "timeout"}
                    break
                if disk_bytes(directory) > limits["bytes"]:
                    result = {"exit_code": 125, "reason": "disk-limit"}
                    break
                if members(child.pid) > limits["processes"]:
                    result = {"exit_code": 125, "reason": "process-limit"}
                    break
                if code is not None:
                    result = {"exit_code": code if code >= 0 else 128 - code,
                              "reason": "completed" if code == 0 else "command-failed"}
                    break
                time.sleep(.2)
        finally:
            try:
                stop(child)
            except (OSError, RuntimeError, subprocess.SubprocessError) as error:
                result["cleanup_error"] = type(error).__name__
                if result["exit_code"] == 0:
                    result.update(exit_code=125, reason="cleanup-failed")
    result["elapsed_seconds"] = round(time.monotonic() - started, 3)
    return result


def allowed(relative):
    if any(part.startswith(".") or part in {"library", "staging", "output", "bundle"}
           for part in relative.parts):
        return False
    name = relative.name
    if "bootstrap-evidence" in relative.parts:
        return name in {"voom-api.stdout", "voom-api.stderr", "voom-node-agent.stdout",
                        "voom-node-agent.stderr", "shutdown.json"}
    return (name in NAMES or name.endswith("-generated.yaml") or
            name.endswith("-semantic.json") or name.startswith("process-") and
            name.endswith((".stdout", ".stderr")) or
            "reports" in relative.parts and relative.suffix == ".json")


def versions(directory, env, interrupted):
    observed = {}
    commands = [(name, [name, flag]) for name, flag in
                (("uv", "--version"), ("python3", "--version"), ("ffmpeg", "-version"),
                 ("ffprobe", "-version"), ("mkvmerge", "--version"), ("cargo", "--version"))]
    commands.append(("chaos-python", ["uv", "run", "--offline", "--no-sync", "--project",
                                      str(UPSTREAM), "python", "--version"]))
    commands.append(("chaos-librarian", ["git", "-C", str(UPSTREAM), "rev-parse", "HEAD"]))
    for program, command in commands:
        path = directory / program
        path.mkdir()
        try:
            status = execute(command, path,
                             {"seconds": 2, "bytes": MIB, "processes": 8}, env, interrupted)
            text = (path / "command.stdout").read_bytes()[:2048]
            observed[program] = {"exit_code": status["exit_code"],
                                 "version": text.decode("utf-8", "replace").splitlines()[:1]}
        except OSError as error:
            observed[program] = {"error": type(error).__name__}
        finally:
            shutil.rmtree(path)
    return observed


def collect(work, receipt, budget, env, interrupted):
    bundle = work / "bundle"
    bundle.mkdir(mode=0o700)
    receipt.update(files=[], omitted=[], runs=[], visualizations=[])
    remaining = budget - min(budget // 4, 4 * MIB)  # Reserve the bounded index.
    sources = []
    for directory, dirs, files in os.walk(work, followlinks=False):
        dirs.sort()
        sources.extend(Path(directory) / name for name in sorted(files))
        sources.extend(Path(directory) / name for name in dirs
                       if (Path(directory) / name).is_symlink())
    for path in sorted(sources):
        relative = path.relative_to(work)
        if not allowed(relative):
            continue
        try:
            # Recheck every parent after walking; no link may turn into an upload source.
            if (path.is_symlink() or not path.resolve().is_relative_to(work) or
                    any((work / parent).is_symlink() for parent in relative.parents)):
                raise ValueError("symlink-or-outside-root")
            info = path.lstat()
            if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
                raise ValueError("special-or-linked-file")
            size = info.st_size
            if size > remaining:
                receipt["omitted"].append({"path": str(relative), "reason": "bundle-limit"})
                continue
            data = path.read_bytes()
            if SECRET.search(data):
                receipt["omitted"].append({"path": str(relative), "reason": "secret-pattern"})
                continue
            if len(data) > remaining:
                raise ValueError("file-grew")
            destination = bundle / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(data)
            remaining -= len(data)
            receipt["files"].append({"path": str(relative), "bytes": len(data),
                                     "sha256": hashlib.sha256(data).hexdigest()})
            if relative.name == "replay.json":
                replay = json.loads(data)
                if not isinstance(replay, dict):
                    raise ValueError("replay-not-an-object")
                receipt["runs"].append({"path": str(relative.parent),
                                        "seed": replay.get("resolved_seed"),
                                        "applied_events": replay.get("applied_events"),
                                        "missing": [name for name in ("scenario.yaml", "manifest.initial.json",
                                                    "manifest.current.json", "journal.jsonl",
                                                    "observed-state.json", "comparison.json")
                                                    if not (bundle / relative.parent / name).is_file()]})
        except (OSError, ValueError) as error:
            receipt["omitted"].append({"path": str(relative), "reason": type(error).__name__})
    # Visualization is offline and best-effort, with a total ten-second diagnostic budget.
    deadline = time.monotonic() + 10
    for run in receipt["runs"]:
        if interrupted[0] or remaining <= 0 or time.monotonic() >= deadline:
            break
        directory = Path(tempfile.mkdtemp(prefix="visualizer-", dir=work))
        try:
            output = directory / "visualize.html"
            status = execute(["uv", "run", "--offline", "--no-sync", "python",
                              "scripts/visualize_run.py", str(work / run["path"]),
                              "--output", str(output)], directory,
                             {"seconds": max(.01, deadline - time.monotonic()),
                              "bytes": remaining, "processes": 8}, env, interrupted,
                             cwd=UPSTREAM)
            receipt["visualizations"].append({"run": run["path"], **status})
            if output.is_file() and not output.is_symlink():
                data = output.read_bytes()
                if len(data) <= remaining and not SECRET.search(data):
                    target = bundle / run["path"] / "visualize.html"
                    target.write_bytes(data)
                    remaining -= len(data)
                    receipt["files"].append({"path": str(target.relative_to(bundle)),
                                             "bytes": len(data),
                                             "sha256": hashlib.sha256(data).hexdigest()})
                else:
                    receipt["omitted"].append({"path": run["path"] + "/visualize.html",
                                               "reason": "bundle-limit-or-secret"})
        except (OSError, subprocess.SubprocessError) as error:
            receipt["visualizations"].append({"run": run["path"], "error": type(error).__name__})
        finally:
            shutil.rmtree(directory)
    receipt["raw_retained"] = receipt.get("mode") == "heavy" and (
        not receipt["runs"] or bool(receipt["omitted"]))
    if receipt.get("mode") == "heavy" and receipt.get("exit_code") == 0 and not receipt["runs"]:
        receipt.update(exit_code=125, reason="missing-replay-evidence")
    receipt["replay_status"] = "available" if receipt["runs"] else "unavailable-before-persistence"
    for run in receipt["runs"]:
        run["missing"] = [name for name in run["missing"]
                          if not (bundle / run["path"] / name).is_file()]


def write_index(bundle, receipt, maximum):
    # Very large inventories cannot enlarge the artifact merely to explain omissions.
    while len((json.dumps(receipt, indent=2) + "\n").encode()) > maximum:
        items = max((receipt[key] for key in ("files", "omitted", "runs", "visualizations")),
                    key=len)
        if not items:
            raise ValueError("diagnostic index exceeds reserved budget")
        items.pop()
        receipt["index_truncated"] = True
    (bundle / "runner.json").write_text(json.dumps(receipt, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["local", "heavy"])
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("a command is required after --")
    try:
        if not shutil.which("ps"):
            raise ValueError("ps is required for bounded process accounting")
        seed, lane = os.environ.get("CHAOS_SEED"), os.environ.get("CHAOS_LANE")
        if seed is not None and not re.fullmatch(r"[0-9]{1,20}", seed):
            raise ValueError("CHAOS_SEED must contain 1..20 ASCII digits")
        if lane is not None and not re.fullmatch(r"[a-z][a-z0-9-]{0,63}", lane):
            raise ValueError("CHAOS_LANE must be a lowercase lane name")
        limits = {"seconds": integer("CHAOS_MAX_SECONDS", 2700 if args.mode == "heavy" else 900, 86400),
                  "bytes": integer("CHAOS_MAX_MIB", 4096, 1048576) * MIB,
                  "processes": integer("CHAOS_MAX_PROCESSES", 128, 4096)}
        budget = integer("CHAOS_BUNDLE_MIB", 64, 1024) * MIB
        work = Path(os.environ.get("CHAOS_WORKDIR") or tempfile.mkdtemp(prefix="voom-chaos-", dir=REPO / ".test-tmp"))
        if work.is_symlink() or work.exists() and any(work.iterdir()):
            raise ValueError("CHAOS_WORKDIR must be new or empty and not a symlink")
        work.mkdir(parents=True, exist_ok=True, mode=0o700)
        work = work.resolve()
        work.chmod(0o700)
    except (ValueError, OSError) as error:
        parser.error(str(error))
    evidence = work / "evidence"
    evidence.mkdir(mode=0o700)
    env = dict(os.environ, CHAOS_SUPERVISED="1", CHAOS_WORKDIR=str(work),
               CHAOS_EVIDENCE_ROOT=str(evidence), VOOM_ACCEPTANCE_PROCESS_GROUP="inherit")
    interrupted = [0]
    for kind in (signal.SIGINT, signal.SIGTERM):
        signal.signal(kind, lambda received, _frame: interrupted.__setitem__(0, 128 + received))
    overall_started = time.monotonic()
    receipt = {"mode": args.mode, "requested_seed": seed,
               "requested_lane": lane, "requested_profile": "fuzz-regression" if seed else None,
               "limits": limits, "bundle_bytes": budget}
    try:
        receipt.update(execute(command, work, limits, env, interrupted))
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        receipt.update(exit_code=125, reason="runner-error", error=type(error).__name__)
    original = receipt["exit_code"]
    receipt["command_exit_code"] = original
    try:
        receipt["versions"] = versions(evidence, env, interrupted)
        pin = receipt["versions"]["chaos-librarian"].get("version", [])
        receipt["pin"] = pin[0] if pin and re.fullmatch(r"[0-9a-f]{40}", pin[0]) else None
        collect(work, receipt, budget, env, interrupted)
        if original == 0:
            original = receipt["exit_code"]
        if args.mode == "heavy" and not receipt["raw_retained"]:
            shutil.rmtree(evidence)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"diagnostic collection failed: {type(error).__name__}", file=sys.stderr)
        receipt["diagnostic_error"] = type(error).__name__
        if original == 0:
            original = 125
    receipt["exit_code"] = original
    receipt["total_elapsed_seconds"] = round(time.monotonic() - overall_started, 3)
    try:
        (work / "bundle").mkdir(mode=0o700, exist_ok=True)
        write_index(work / "bundle", receipt, min(budget // 4, 4 * MIB))
    except OSError as error:
        print(f"diagnostic index failed: {type(error).__name__}", file=sys.stderr)
        original = original or 125
    for name in ("command.stdout", "command.stderr"):
        with (work / name).open("rb") as log:
            log.seek(max(0, (work / name).stat().st_size - 32768))
            data = log.read(32768)
        if not SECRET.search(data):
            sys.stderr.buffer.write(data[-32768:])
    print(f"chaos {receipt['reason']} (exit {original}); diagnostics: {work / 'bundle'}",
          file=sys.stderr)
    return original


if __name__ == "__main__":
    sys.exit(main())
