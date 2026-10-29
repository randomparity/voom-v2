#!/usr/bin/env python3
"""Prove copied audio preservation and rejection through the authenticated CLI."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parents[1]
CODECS = ("aac", "opus", "eac3", "ac3", "flac", "alac")


def run(arguments, cwd=None):
    result = subprocess.run(list(map(str, arguments)), cwd=cwd, capture_output=True,
                            timeout=90, env={**os.environ,
                            "VOOM_MEDIA_CALLER_PID": str(os.getpid())})
    if result.returncode:
        raise RuntimeError(f"{arguments[0]} failed ({result.returncode}): "
                           + result.stderr.decode(errors="replace"))
    return result.stdout


def probe(executable, path, cwd):
    return json.loads(run([executable, "-v", "error", "-show_format", "-show_streams",
                          "-show_packets", "-show_data_hash", "sha256", "-of", "json", path], cwd))


def tuples(value, index):
    return [tuple(packet.get(key) for key in ("data_hash", "pts", "dts", "duration"))
            for packet in value["packets"] if packet["stream_index"] == index]


def generate(root, codec, synthesis, video, short):
    name = f"{codec}-{int(synthesis)}"
    scratch = root / "scratch" / name
    scratch.mkdir()
    selected = scratch / "main.mkv"
    run(["ffmpeg", "-v", "error", "-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo",
         "-af", "atrim=end_sample=700", "-c:a", "alac" if codec == "flac" else "flac",
         *([] if codec == "flac" else ["-frame_size", "4608"]),
         "-metadata:s:a:0", "FIXTURE_USER_NOTE=preserve copied audio metadata", selected])
    source = root / "input" / name
    source.mkdir()
    media = source / f"{name}.mkv"
    run(["mkvmerge", "-o", media, "--title", "Copied audio timing", video,
         "--language", "0:eng", "--track-name", "0:Main", "--default-track-flag", "0:yes",
         "--forced-display-flag", "0:yes", "--commentary-flag", "0:yes", selected,
         "--language", "0:jpn", "--track-name", "0:Untouched short FLAC",
         "--default-track-flag", "0:no", short])
    return media


def selection(stack, source, codec, synthesis, directory):
    name = f"timing-{codec}-{int(synthesis)}"
    root = next(item for item in stack.roots if item["provider_locator"] == str(source.parent))
    scanned = stack.voom("scan", "--root", root["root_id"])["data"]
    assert scanned["status"] == "succeeded" and scanned["observation_count"] == 1, scanned
    operation = (f'synthesize audio from language in ["eng"] and channels >= 2 '
                 f'{{ codec {codec} channels 1 }}' if synthesis
                 else f'transcode audio to {codec} where language in ["eng"]')
    policy = directory / f"{name}.voom"
    policy.write_text(f'policy "{name}" {{ phase encode {{ {operation} }} }}\n')
    version = stack.voom("policy", "create", "--slug", name, "--file", policy)["data"][
        "version"]["version_id"]
    inputs = stack.voom("policy", "input", "create-from-scan", "--root", root["root_id"],
                        "--slug", name)["data"]["input_set"]
    assert inputs["included_count"] == 1, inputs
    return ["--policy-version-id", version, "--input-set-id", inputs["input_set_id"]]


def check_output(before, after, synthesis):
    assert len(after["streams"]) == 3 + int(synthesis)
    assert tuples(before, 2)[0][-1] == 15, "fixture must retain explicit rounded duration"
    for index in ((0, 1, 2) if synthesis else (0, 2)):
        assert before["streams"][index]["time_base"] == after["streams"][index]["time_base"]
        assert tuples(before, index) and tuples(before, index) == tuples(after, index)
        assert before["streams"][index]["disposition"] == after["streams"][index]["disposition"]
    # Check fixture-authored user metadata independently of generated mux bookkeeping.
    for index in (1, 2):
        expected = {key.lower(): value for key, value in before["streams"][index]["tags"].items()}
        actual = {key.lower(): value for key, value in after["streams"][index]["tags"].items()}
        for key in ("language", "title", "fixture_user_note"):
            if key in expected:
                assert expected[key] == actual.get(key), (index, key)
    assert after["format"]["tags"]["title"] == before["format"]["tags"]["title"]
    if synthesis:
        assert after["streams"][3]["channels"] == 1
        assert after["streams"][3]["disposition"]["default"] == 0


def positive(stack, source, codec, synthesis, tools, stage, output, root):
    original = source.stat().st_mtime_ns, hashlib.sha256(source.read_bytes()).hexdigest()
    before = probe(tools["ffprobe_bin"], source, stage)
    select = selection(stack, source, codec, synthesis, root)
    planned = stack.voom("plan", "show", *select)["data"]["plan"]["summary"]
    assert planned["executable_node_count"] == 1 and planned["blocked_node_count"] == 0, planned
    previous = set(output.rglob("*.mkv"))
    result = stack.voom("compliance", "execute", *select, "--staging-root", stage,
                        "--output-dir", output)["data"]
    assert result["summary"]["failure_count"] == 0, result
    phase = result["file_phases"][0]
    assert phase["outcome"] == "committed" and phase["produced_file_version_id"] > 0, result
    produced = set(output.rglob("*.mkv")) - previous
    assert len(produced) == 1, produced
    check_output(before, probe(tools["ffprobe_bin"], produced.pop(), stage), synthesis)
    if synthesis:
        with sqlite3.connect(f"file:{stack.workdir}/voom.db?mode=ro", uri=True) as db:
            rows = db.execute(
                "SELECT source_provider_stream_index,result_provider_stream_index,codec,channels,"
                "disposition_default,source_file_version_id,result_file_version_id "
                "FROM audio_synthesis_stream_lineage WHERE result_file_version_id=?",
                (phase["produced_file_version_id"],)).fetchall()
        assert len(rows) == 1 and rows[0][:5] == (1, 3, codec, 1, 0), rows
        assert rows[0][5] != rows[0][6]
    assert original == (source.stat().st_mtime_ns, hashlib.sha256(source.read_bytes()).hexdigest())
    print(json.dumps({"codec": codec, "synthesis": synthesis, "committed_tuples": "exact"}), flush=True)


def negative(stack, source, stage, output, root, ffmpeg, ffprobe):
    assert source.parent == root / "input", "native negative must use its isolated source root"
    original = hashlib.sha256(source.read_bytes()).hexdigest()
    before = probe(ffprobe, source, stage)
    control = stage / "native-control.mkv"
    run([ffmpeg, "-v", "error", "-copyts", "-i", source, "-map", "0", "-c", "copy",
         "-c:a:0", "alac", "-avoid_negative_ts", "disabled", control], stage)
    after = probe(ffprobe, control, stage)
    source_packet, copied_packet = tuples(before, 2)[0], tuples(after, 2)[0]
    assert source_packet[:-1] == copied_packet[:-1]
    assert source_packet[-1] == 15 and copied_packet[-1] == 14, (source_packet, copied_packet)
    select = selection(stack, source, "alac", False, root)
    try:
        stack.voom("compliance", "execute", *select, "--staging-root", stage,
                   "--output-dir", output)
    except RuntimeError as error:
        assert "exit 2" in str(error), error
        envelope = json.loads((stack.evidence / f"cli-{stack.counter:04}.json").read_text())
        assert envelope["status"] == "error", envelope
        with sqlite3.connect(f"file:{stack.workdir}/voom.db?mode=ro", uri=True) as db:
            failures = db.execute(
                "SELECT e.payload FROM events e JOIN tickets t "
                "ON json_extract(e.payload,'$.ticket_id')=t.id "
                "WHERE e.kind='ticket.failed_terminal' AND t.job_id=?",
                (envelope["data"]["summary"]["job_id"],)).fetchall()
            output_id = next(item["root_id"] for item in stack.roots if item["role"] == "output")
            locations = db.execute("SELECT COUNT(*) FROM file_locations WHERE storage_root_id=?",
                                   (output_id,)).fetchone()[0]
        assert len(failures) == 1 and "copied audio preservation" in json.loads(failures[0][0])["reason"]
        assert locations == 0, "unsafe result acquired a published output location"
    else:
        raise AssertionError("native rounded copied packet unexpectedly became publishable")
    assert not list(output.rglob("*.mkv")), "unsafe copied packet was published"
    assert original == hashlib.sha256(source.read_bytes()).hexdigest(), "native source changed"
    print(json.dumps({"native_encoder_project_probe": "rejected-before-publication",
                      "observed_duration": [15, 14]}), flush=True)


def regression(args, root):
    native = {name: shutil.which(name) for name in ("ffmpeg", "mkvmerge")}
    if not all(native.values()):
        raise RuntimeError("install native ffmpeg and mkvtoolnix before this proof")
    for name in ("scratch", "input", "staging", "output"):
        (root / name).mkdir()
    rejected = root / "native"
    rejected.mkdir()
    for name in ("input", "staging", "output"):
        (rejected / name).mkdir()
    scratch, stage, output = (root / name for name in ("scratch", "staging", "output"))
    video, short = scratch / "video.mkv", scratch / "short.flac"
    run([native["ffmpeg"], "-v", "error", "-f", "lavfi", "-i", "color=size=16x16:rate=10",
         "-frames:v", "1", "-c:v", "ffv1", video])
    run([native["ffmpeg"], "-v", "error", "-f", "lavfi", "-i", "anullsrc=r=48000:cl=mono",
         "-af", "atrim=end_sample=700", "-c:a", "flac", "-frame_size", "4608", short])
    sources = {(codec, synthesis): generate(root, codec, synthesis, video, short)
               for codec in CODECS for synthesis in (False, True)}
    source = rejected / "input/source.mkv"
    shutil.copyfile(sources[("alac", False)], source)
    config = root / "container.json"
    configuration = {"engine": args.engine, "image": args.image,
                     "read_only_roots": [str(root / "input"), str(rejected / "input")],
                     "read_write_roots": [str(stage), str(output),
                                          str(rejected / "staging"), str(rejected / "output")]}
    if args.desktop:
        configuration["desktop"] = args.desktop
    config.write_text(json.dumps(configuration))
    tools = json.loads(run([sys.executable, REPO / "containers/ffmpeg/install.py", "--config", config,
                           "--destination", root / "installed", "--source-witness", sources[("aac", False)],
                           "--source-witness", source], stage))
    spec = importlib.util.spec_from_file_location("bootstrap", REPO / "scripts/acceptance-bootstrap.py")
    bootstrap = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bootstrap)
    arguments = ["serve", "--workdir", str(root / "stack"), "--staging", str(stage),
                 "--output", str(output), "--bin-dir", str(args.bin_dir)]
    for input_source in sources.values():
        arguments += ["--input", str(input_source.parent)]
    previous_directory = Path.cwd()
    try:
        os.environ["VOOM_FFMPEG_BIN"] = tools["ffmpeg_bin"]
        os.environ["VOOM_FFPROBE_BIN"] = tools["ffprobe_bin"]
        os.chdir(stage)
        with bootstrap.Bootstrap(bootstrap.parser().parse_args(arguments)) as stack:
            for (codec, synthesis), positive_source in sources.items():
                positive(stack, positive_source, codec, synthesis, tools, stage, output, root)
        os.environ["VOOM_FFMPEG_BIN"] = native["ffmpeg"]
        # Older native probes hide this fixture's explicit Matroska BlockDuration.
        os.environ["VOOM_FFPROBE_BIN"] = tools["ffprobe_bin"]
        os.chdir(rejected / "staging")
        arguments = ["serve", "--workdir", str(rejected / "stack"), "--input", str(source.parent),
                     "--staging", str(rejected / "staging"), "--output", str(rejected / "output"),
                     "--bin-dir", str(args.bin_dir)]
        with bootstrap.Bootstrap(bootstrap.parser().parse_args(arguments)) as stack:
            negative(stack, source, rejected / "staging", rejected / "output", rejected,
                     native["ffmpeg"], tools["ffprobe_bin"])
    finally:
        os.chdir(previous_directory)
    print(json.dumps({"status": "ok", "project_cases": 12, "native_rejections": 1, "ignored": 0}), flush=True)


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", choices=("docker", "podman"), required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--desktop", help="existing macOS Docker context or Podman machine")
    parser.add_argument("--workdir", type=Path)
    parser.add_argument("--bin-dir", type=Path,
                        default=Path(os.environ.get("CARGO_TARGET_DIR", REPO / "target")) / "debug")
    args = parser.parse_args()
    args.bin_dir = args.bin_dir.resolve()
    if args.workdir:
        root = args.workdir.resolve()
        root.mkdir(parents=True, mode=0o700, exist_ok=False)
        regression(args, root)
    else:
        with tempfile.TemporaryDirectory(prefix="copied-audio-786-", dir=REPO / ".test-tmp") as root:
            regression(args, Path(root).resolve())


if __name__ == "__main__":
    main()
