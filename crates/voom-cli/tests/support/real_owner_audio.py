"""Generated MKV audio acceptance through the authenticated CLI/owner stack."""

import argparse
import array
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import sqlite3
import struct
import subprocess
import sys
import tempfile

CODECS = ("aac", "opus", "eac3", "ac3", "flac", "alac", "pcm_s16le", "pcm_s24le")
ARCHIVAL = {"flac", "alac", "pcm_s16le", "pcm_s24le"}
REPO = Path(__file__).resolve().parents[4]
POLICIES = REPO / "crates/voom-control-plane/tests/fixtures/policies/audio-acceptance"


def run(arguments, cwd=None):
    result = subprocess.run(
        list(map(str, arguments)), cwd=cwd,
        env={**os.environ, "VOOM_MEDIA_CALLER_PID": str(os.getpid())},
        capture_output=True, timeout=90, check=False,
    )
    if result.returncode:
        raise RuntimeError(f"{arguments[0]} failed ({result.returncode}): "
                           + result.stderr.decode(errors="replace"))
    return result.stdout


def probe(ffprobe, path, stage):
    return json.loads(run([
        ffprobe, "-v", "error", "-show_format", "-show_streams", "-show_packets",
        "-show_data_hash", "sha256", "-of", "json", path,
    ], stage))


def integer_wave(path, bits, channels=6, frames=48000, mask=None):
    mask = mask or {1: 4, 2: 3, 6: 0x3F, 8: 0x63F}[channels]
    limit = 1 << (bits - 1)
    values = (-limit, limit - 1, -1, 1, 0, 123, -123, 3)
    data = bytearray()
    for frame in range(frames):
        for channel in range(channels):
            # LFE is bandwidth limited by the lossy codecs; keep its witness in-band.
            frequency = 80 if channels >= 6 and channel == 3 else 220 + 110 * channel
            value = values[(frame + channel) % len(values)] if frame < 8 else (
                int(limit / 8 * math.sin(2 * math.pi * frequency * frame / 48000))
                + (frame + channel) % 7 - 3)
            data.extend(value.to_bytes(4, "little", signed=True)[:bits // 8])
    fmt = struct.pack("<HHIIHHHHI", 0xFFFE, channels, 48000,
                      48000 * channels * bits // 8, channels * bits // 8,
                      bits, 22, bits, mask)
    fmt += bytes.fromhex("0100000000001000800000aa00389b71")
    body = b"WAVEfmt " + struct.pack("<I", len(fmt)) + fmt
    body += b"data" + struct.pack("<I", len(data)) + data
    path.write_bytes(b"RIFF" + struct.pack("<I", len(body)) + body)


def generate(root, name, source_codec, bits, video, short, subtitles, channels=6, mask=None):
    scratch = root / "scratch" / name
    scratch.mkdir(parents=True)
    wave = scratch / "integer.wav"
    integer_wave(wave, bits, channels, mask=mask)
    audio = scratch / "main.mkv"
    run(["ffmpeg", "-v", "error", "-i", wave, "-c:a", source_codec,
         "-metadata:s:a:0", "FIXTURE_USER_NOTE=preserve user metadata", audio])
    source = root / "input" / name
    source.mkdir(parents=True)
    media = source / f"{name}.mkv"
    run(["mkvmerge", "-o", media, "--title", "Generated audio acceptance", video,
         "--language", "0:eng", "--track-name", "0:Main", "--default-track-flag", "0:yes",
         "--forced-display-flag", "0:yes", "--commentary-flag", "0:yes",
         "--hearing-impaired-flag", "0:yes", "--visual-impaired-flag", "0:yes", audio,
         "--language", "0:jpn", "--track-name", "0:Untouched short audio",
         "--default-track-flag", "0:no", short,
         "--language", "0:eng", "--track-name", "0:Untouched subtitle", subtitles])
    return media


def decode(ffmpeg, path, index, stage, channels=None, bits=32):
    # Same canonical s32le representation as voom-test-support::audio_samples:
    # no resampling/remixing/dither for archival; an explicit independent downmix
    # is requested only for the reference transform.
    args = [ffmpeg, "-nostdin", "-v", "error", "-guess_layout_max", "0", "-i", path,
            "-map", f"0:{index}"]
    if channels:
        args += ["-ac", str(channels)]
    result = run(args + [
        "-c:a", f"pcm_s{bits}le", "-dither_method", "0", "-f", f"s{bits}le", "pipe:1",
    ], stage)
    if bits != 32:
        width = bits // 8
        result = b"".join((int.from_bytes(result[offset:offset + width], "little", signed=True)
                           << (32 - bits)).to_bytes(4, "little", signed=True)
                          for offset in range(0, len(result), width))
    assert result and len(result) % (4 * (channels or 6)) == 0, "empty/incomplete integer decode"
    return result


def tags(stream):
    # DURATION is regenerated mux bookkeeping, not user metadata.
    return {key.lower(): value for key, value in stream.get("tags", {}).items()
            if key.lower() != "duration"}


def packets(probe_result, index, timing=False):
    fields = ("data_hash", "pts", "dts", "duration") if timing else ("data_hash",)
    return [tuple(packet.get(key) for key in fields) for packet in probe_result["packets"]
            if packet["stream_index"] == index]


def check_lossy_signal(ffmpeg, source, output, stage, downmix):
    channels = 2 if downmix else 6
    witnesses = []
    for path, index in ((source, 1), (output, 4 if downmix else 1)):
        decoded = run([ffmpeg, "-v", "error", "-i", path, "-map", f"0:{index}",
                       "-ac", channels, "-c:a", "pcm_f32le", "-f", "f32le", "pipe:1"], stage)
        witnesses.append(array.array("f", decoded))
    expected, actual = witnesses
    if sys.byteorder != "little":
        expected.byteswap()
        actual.byteswap()
    frames = min(len(expected), len(actual)) // channels
    positions = range(4096, frames - 4096, 47)
    assert len(positions) > 500, "lossy signal witness is too short"

    def scores(lag):
        correlations, energies = [], []
        for channel in range(channels):
            pairs = [
                (expected[frame * channels + channel], actual[(frame + lag) * channels + channel])
                for frame in positions
            ]
            original_energy = sum(left * left for left, _ in pairs)
            encoded_energy = sum(right * right for _, right in pairs)
            cross = sum(left * right for left, right in pairs)
            correlations.append(cross / math.sqrt(original_energy * encoded_energy)
                                if encoded_energy else 0)
            energies.append(encoded_energy / original_energy)
        return min(correlations), energies

    # Codec priming is allowed; compare independently decoded waveform channels
    # after bounded alignment, including the requested reference downmix.
    lag = max(range(-2048, 2049, 8), key=lambda candidate: scores(candidate)[0])
    correlation, energies = max((scores(candidate) for candidate in range(lag - 7, lag + 8)),
                                key=lambda result: result[0])
    assert correlation > 0.97, correlation
    assert all(0.7 < energy < 1.3 for energy in energies), energies


def check_preservation(before, after, codec, downmix, timing_differences):
    source, result = before["streams"], after["streams"]
    assert len(result) == len(source) + int(downmix), "unexpected stream set"
    assert after["format"]["tags"]["title"] == before["format"]["tags"]["title"]
    target = result[-1] if downmix else result[1]
    assert target["codec_name"] == codec, target
    assert target["sample_rate"] == "48000", target
    assert target["channels"] == (2 if downmix else 6), target
    expected_disposition = dict(source[1]["disposition"])
    expected_disposition["default"] = 0 if downmix else 1
    assert target["disposition"] == expected_disposition, "selected full dispositions changed"
    expected_tags = tags(source[1])
    expected_tags.pop("encoder", None)
    assert all(tags(target).get(key) == value for key, value in expected_tags.items()), (
        "selected user metadata changed")
    if codec in ARCHIVAL:
        assert target["sample_rate"] == source[1]["sample_rate"]
        assert target["channel_layout"] == ("stereo" if downmix else "5.1")
        expected_bits = int(source[1].get("bits_per_raw_sample") or source[1]["bits_per_sample"])
        if codec == "pcm_s24le":
            expected_bits = 24
        actual_bits = int(target.get("bits_per_raw_sample") or target["bits_per_sample"])
        assert actual_bits == expected_bits, (expected_bits, target)
    copied = range(len(source)) if downmix else (0, 2, 3)
    for index in copied:
        assert result[index]["codec_name"] == source[index]["codec_name"]
        assert tags(result[index]) == tags(source[index]), f"copied stream {index} user metadata"
        assert result[index]["disposition"] == source[index]["disposition"]
        assert packets(before, index), f"copied stream {index} has no packets"
        assert packets(before, index) == packets(after, index), (
            f"copied stream {index} payload changed")
        assert result[index]["time_base"] == source[index]["time_base"], "copied time base changed"
        expected = packets(before, index, True)
        actual = packets(after, index, True)
        if expected != actual:
            timing_differences.append({"codec": codec, "downmix": downmix, "stream": index,
                                       "expected": expected, "actual": actual})


def selection(stack, root, codec, mode, version=None, input_slug=None):
    name = f"acceptance-{codec}-{mode}".replace("_", "-")
    scan = stack.voom("scan", "--root", root["root_id"])["data"]
    assert scan["status"] == "succeeded" and scan["observation_count"] == 1, scan
    policy = version or stack.voom(
        "policy", "create", "--slug", name, "--file", POLICIES / f"{codec}-{mode}.voom",
    )["data"]["version"]["version_id"]
    inputs = stack.voom("policy", "input", "create-from-scan", "--root", root["root_id"],
                        "--slug", input_slug or name)["data"]["input_set"]
    assert inputs["included_count"] == 1, inputs
    return ["--policy-version-id", policy, "--input-set-id", inputs["input_set_id"]]


def check_noop(stack, select, execution, codec, output):
    committed = execution["file_phases"][0]
    compliant = stack.voom(
        "policy", "input", "create-from-scan", "--slug", f"compliant-{codec}".replace("_", "-"),
        "--file-version-id", committed["produced_file_version_id"],
        "--media-snapshot-id", committed["reprobe_snapshot_id"], "--container", "mkv",
        "--video-codec", "ffv1",
    )["data"]["input_set"]["input_set_id"]
    updated = select[:2] + ["--input-set-id", compliant]
    plan = stack.voom("plan", "show", *updated)["data"]["plan"]
    assert plan["summary"]["no_op_node_count"] == 1
    assert plan["summary"]["executable_node_count"] == 0
    assert plan["summary"]["blocked_node_count"] == 0
    assert output.is_file(), "no-op lost the committed file"


def check_lineage(stack, execution, codec):
    phase = execution["file_phases"][0]
    with sqlite3.connect(f"file:{stack.workdir}/voom.db?mode=ro", uri=True) as db:
        rows = db.execute(
            "SELECT source_provider_stream_index,result_provider_stream_index,codec,channels,"
            "disposition_default,source_file_version_id,result_file_version_id "
            "FROM audio_synthesis_stream_lineage WHERE result_file_version_id = ?",
            (phase["produced_file_version_id"],),
        ).fetchall()
    assert len(rows) == 1, rows
    row = rows[0]
    assert row[:5] == (1, 4, codec, 2, 0) and row[5] != row[6], row


def check_archival_samples(ffmpeg, source, output, stage, codec, downmix, source_bits):
    reference_bits = 16 if codec == "pcm_s16le" else source_bits
    expected = decode(ffmpeg, source, 1, stage, 2 if downmix else None,
                      reference_bits if downmix else 32)
    actual = decode(ffmpeg, output, 4 if downmix else 1, stage, 2 if downmix else None)
    assert expected == actual, "canonical decoded integer samples differ"
    if downmix:
        assert decode(ffmpeg, source, 1, stage) == decode(ffmpeg, output, 1, stage)


def execute_case(stack, source, codec, mode, tools, stage, output, timing_differences):
    original = (source.stat().st_mtime_ns, hashlib.sha256(source.read_bytes()).hexdigest())
    before = probe(tools["ffprobe_bin"], source, stage)
    assert before["streams"][2]["time_base"] == "1/1000", "missing timing sentinel base"
    assert [packet[3] for packet in packets(before, 2, True)] == [15], (
        "short FLAC must expose the 15 ms copied-duration sentinel")
    root = next(root for root in stack.roots if root["provider_locator"] == str(source.parent))
    select = selection(stack, root, codec, mode)
    plan = stack.voom("plan", "show", *select)["data"]["plan"]
    assert plan["summary"]["executable_node_count"] == 1
    assert plan["summary"]["blocked_node_count"] == 0
    previous = set(output.rglob("*.mkv"))
    execution = stack.voom("compliance", "execute", *select, "--staging-root", stage,
                           "--output-dir", output)["data"]
    assert execution["summary"]["failure_count"] == 0, execution
    assert execution["summary"]["per_operation"]["transcode_audio"]["success_count"] == 1
    assert len(execution["file_phases"]) == 1
    phase = execution["file_phases"][0]
    assert phase["outcome"] == "committed" and phase["ticket_ids"]
    assert all(phase[key] > 0 for key in (
        "produced_file_version_id", "produced_file_location_id", "reprobe_snapshot_id"))
    produced = set(output.rglob("*.mkv")) - previous
    assert len(produced) == 1, produced
    actual = produced.pop()
    after = probe(tools["ffprobe_bin"], actual, stage)
    downmix = mode == "downmix"
    check_preservation(before, after, codec, downmix, timing_differences)
    if codec in ARCHIVAL:
        check_archival_samples(tools["ffmpeg_bin"], source, actual, stage, codec, downmix,
                               int(before["streams"][1]["bits_per_raw_sample"]))
    else:
        check_lossy_signal(tools["ffmpeg_bin"], source, actual, stage, downmix)
    if downmix:
        check_lineage(stack, execution, codec)
    else:
        check_noop(stack, select, execution, codec, actual)
    assert original == (
        source.stat().st_mtime_ns, hashlib.sha256(source.read_bytes()).hexdigest()
    ), "source changed"
    print(json.dumps({
        "case": f"{codec}-{mode}", "status": "checked", "committed": True,
    }), flush=True)
    return select


def execute_negative(stack, source, codec, version, diagnostic, stage, output, name):
    original = hashlib.sha256(source.read_bytes()).hexdigest()
    facts = probe(os.environ["VOOM_FFPROBE_BIN"], source, stage)["streams"][1]
    if name == "missing-layout-facts":
        assert not facts.get("channel_layout"), facts
    elif name == "floating-sample-format":
        assert facts["sample_fmt"].startswith("flt"), facts
    elif name == "unsupported-ac3-channels":
        assert facts["channels"] == 8 and facts["channel_layout"] == "7.1", facts
    elif name == "unsupported-alac-layout":
        assert facts["channel_layout"] == "5.1(side)", facts
    else:
        assert facts["bits_per_raw_sample"] == "24" and facts["channel_layout"] == "5.1", facts
    root = next(root for root in stack.roots if root["provider_locator"] == str(source.parent))
    select = selection(stack, root, codec, "transcode", version, name)
    existing = set(stage.rglob("*")) | set(output.rglob("*"))
    try:
        stack.voom(
            "compliance", "execute", *select, "--staging-root", stage, "--output-dir", output)
    except RuntimeError as error:
        assert "exit 2" in str(error), error
        evidence = json.loads((stack.evidence / f"cli-{stack.counter:04}.json").read_text())
        assert evidence["status"] == "error", evidence
        with sqlite3.connect(f"file:{stack.workdir}/voom.db?mode=ro", uri=True) as db:
            failures = db.execute(
                "SELECT e.payload FROM events e JOIN tickets t "
                "ON json_extract(e.payload,'$.ticket_id') = t.id "
                "WHERE e.kind='ticket.failed_terminal' AND t.job_id = ?",
                (evidence["data"]["summary"]["job_id"],),
            ).fetchall()
        assert len(failures) == 1, failures
        assert diagnostic in json.loads(failures[0][0])["reason"].lower(), failures
    else:
        raise AssertionError(f"{name} unexpectedly succeeded")
    assert original == hashlib.sha256(source.read_bytes()).hexdigest(), "rejected source changed"
    created = (set(stage.rglob("*")) | set(output.rglob("*"))) - existing
    assert not any(path.is_file() for path in created), f"{name} created media before rejection"
    print(json.dumps({"case": name, "status": "rejected-before-output"}), flush=True)


def without_flac_encoder(root, executable):
    wrapper = root / "ffmpeg-without-flac"
    wrapper.write_text(f'''#!{sys.executable}
import os, subprocess, sys
executable = {executable!r}
if "-encoders" in sys.argv[1:]:
    result = subprocess.run([executable, *sys.argv[1:]], capture_output=True, timeout=60,
                            env={{**os.environ, "VOOM_MEDIA_CALLER_PID": str(os.getpid())}})
    for line in result.stdout.splitlines(keepends=True):
        fields = line.split()
        if len(fields) < 2 or fields[1] != b"flac":
            sys.stdout.buffer.write(line)
    sys.stderr.buffer.write(result.stderr)
    sys.exit(result.returncode)
os.execv(executable, [executable, *sys.argv[1:]])
''')
    wrapper.chmod(0o700)
    return wrapper


def acceptance(args, root):
    for name in ("input", "scratch", "staging", "output"):
        (root / name).mkdir()
    scratch, stage, output = (root / name for name in ("scratch", "staging", "output"))
    video, short, subtitles = (
        scratch / name for name in ("video.mkv", "short.mkv", "subtitle.srt"))
    run(["ffmpeg", "-v", "error", "-f", "lavfi", "-i",
         "color=size=64x64:rate=5:duration=1", "-c:v", "ffv1", video])
    short_wave = scratch / "short.wav"
    integer_wave(short_wave, 16, channels=1, frames=700)
    run(["ffmpeg", "-v", "error", "-i", short_wave, "-c:a", "flac", "-frame_size", "4608", short])
    subtitles.write_text("1\n00:00:00,000 --> 00:00:00,700\nGenerated acceptance subtitle\n")
    cases = [(codec, mode) for codec in CODECS for mode in ("transcode", "downmix")]
    expected = {f"{codec}-{mode}" for codec in
                ("aac", "opus", "eac3", "ac3", "flac", "alac", "pcm_s16le", "pcm_s24le")
                for mode in ("transcode", "downmix")}
    assert {f"{codec}-{mode}" for codec, mode in cases} == expected and len(cases) == 16
    assert {p.stem for p in POLICIES.glob("*.voom")} == expected, (
        "committed policy inventory changed")
    sources = [generate(
        root, f"{codec}-{mode}", "alac" if codec == "flac" else "flac",
        16 if codec == "pcm_s16le" else 24, video, short, subtitles,
    ) for codec, mode in cases]
    negatives = [
        ("pcm-precision-reduction", "pcm_s16le", "flac", 24, 6, "precision"),
        ("missing-layout-facts", "flac", "pcm_s16le", 16, 6, "layout"),
        ("floating-sample-format", "flac", "pcm_f32le", 24, 6, "integer"),
        ("unsupported-ac3-channels", "ac3", "flac", 24, 8, "ac-3"),
        ("unsupported-alac-layout", "alac", "flac", 24, 6, "alac"),
    ]
    negative_names = {"pcm-precision-reduction", "missing-layout-facts", "floating-sample-format",
                      "unsupported-ac3-channels", "unsupported-alac-layout", "absent-flac-encoder"}
    assert {case[0] for case in negatives} | {"absent-flac-encoder"} == negative_names
    assert len(negatives) == 5
    negative_sources = [generate(root, name, source_codec, bits, video, short, subtitles, channels,
                                 0x60F if name == "unsupported-alac-layout" else None)
                        for name, _, source_codec, bits, channels, _ in negatives]
    missing_encoder = generate(root, "absent-flac-encoder", "alac", 24, video, short, subtitles)
    config = root / "container.json"
    configuration = {"engine": args.engine, "image": args.image,
                     "read_only_roots": [str(root / "input")],
                     "read_write_roots": [str(stage), str(output)]}
    if args.desktop:
        configuration["desktop"] = args.desktop
    config.write_text(json.dumps(configuration))
    tools = json.loads(run([
        sys.executable, REPO / "containers/ffmpeg/install.py", "--config", config,
        "--destination", root / "installed", "--source-witness", sources[0],
    ], stage))
    spec = importlib.util.spec_from_file_location(
        "bootstrap", REPO / "scripts/acceptance-bootstrap.py")
    bootstrap = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bootstrap)
    setup = ["serve", "--workdir", str(root / "stack"), "--staging", str(stage),
             "--output", str(output), "--bin-dir", str(args.bin_dir)]
    for source in sources + negative_sources:
        setup += ["--input", str(source.parent)]
    configuration = bootstrap.parser().parse_args(setup)
    os.environ["VOOM_FFMPEG_BIN"] = tools["ffmpeg_bin"]
    os.environ["VOOM_FFPROBE_BIN"] = tools["ffprobe_bin"]
    timing_differences = []
    completed = []
    previous_directory = Path.cwd()
    try:
        os.chdir(stage)
        with bootstrap.Bootstrap(configuration) as stack:
            selections = {}
            for (codec, mode), source in zip(cases, sources, strict=True):
                selections[(codec, mode)] = execute_case(
                    stack, source, codec, mode, tools, stage, output, timing_differences)
                completed.append(f"{codec}-{mode}")
            for case, source in zip(negatives, negative_sources, strict=True):
                name, codec, _, _, _, diagnostic = case
                execute_negative(stack, source, codec, selections[(codec, "transcode")][1],
                                 diagnostic, stage, output, name)
                completed.append(name)
        absent_setup = ["serve", "--workdir", str(root / "absent-stack"), "--staging", str(stage),
                        "--output", str(output), "--input", str(missing_encoder.parent),
                        "--bin-dir", str(args.bin_dir), "--ffmpeg-bin",
                        str(without_flac_encoder(root, tools["ffmpeg_bin"]))]
        with bootstrap.Bootstrap(bootstrap.parser().parse_args(absent_setup)) as stack:
            execute_negative(stack, missing_encoder, "flac", None,
                             "encoder `flac` is not available", stage, output,
                             "absent-flac-encoder")
            completed.append("absent-flac-encoder")
    finally:
        os.chdir(previous_directory)
    assert len(completed) == 22 and set(completed) == expected | negative_names, completed
    (root / "timing-differences.json").write_text(json.dumps(timing_differences, indent=2) + "\n")
    assert not timing_differences, f"copied packet timing differs: {json.dumps(timing_differences)}"
    print(json.dumps({
        "status": "ok", "executed_cases": sorted(completed), "ignored": 0,
    }), flush=True)


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
        root.mkdir(mode=0o700, parents=True, exist_ok=False)
        acceptance(args, root)
    else:
        with tempfile.TemporaryDirectory(
            prefix="audio-acceptance-", dir=REPO / ".test-tmp"
        ) as temporary:
            acceptance(args, Path(temporary).resolve())


if __name__ == "__main__":
    main()
