"""Pinned structural recipes; fixture facts never supply VOOM observations."""

import hashlib
import json
import re
import shutil
import sys
from pathlib import Path

from ruamel.yaml import YAML

from chaos_librarian.materializer import materialize_scenario
from chaos_librarian.materializer.tooling._subprocess import run_recorded_tool


CELLS = [("mp4-start", "mp4", "moov_at_start"),
         ("mp4-end", "mp4", "moov_at_end"),
         ("mkv-cues", "mkv", None), ("mp3-16", "mp3", None)]


def tool(argv):
    result = run_recorded_tool(
        argv, tool=argv[0], version="test", timeout_s=15,
        stdout_mode="pipe", text=False, max_output_bytes=1024 * 1024,
    )
    assert result.failure_kind is None, result.stderr_tail
    return result


def packets(path):
    result = tool(["ffprobe", "-v", "error", "-select_streams", "a:0",
                   "-show_packets", "-show_entries", "packet=pos,size", "-of", "json", str(path)])
    assert result.invocation.exit_code == 0, result.stderr_tail
    return json.loads(result.stdout_text())["packets"]


def scenario(directory, container, placement, damaged):
    # Same bounded recipes as pinned tests/integration/test_structural_corruption_real.py.
    source = "audio-only-track" if container == "mp3" else "malformed-container-header"
    raw = YAML(typ="safe").load(Path(f"tests/fixtures/scenarios/{source}.yaml").read_text())
    raw["profiles"] = ["malformed-media"]
    if container == "mp3":
        asset = raw["artists"][0]["albums"][0]["discs"][0]["tracks"][0]["variants"][0][
            "bundle"
        ]["assets"][0]
        asset["audio"][0].update(codec="mp3", sample_rate=44100)
    else:
        asset = raw["movies"][0]["variants"][0]["bundle"]["assets"][0]
        asset["video"]["resolution"] = "sd"
        if placement:
            asset["mp4_moov_placement"] = placement
    asset.update(container=container, duration_seconds=2)
    event = {"id": "structural", "at": "1ms", "target": asset["id"]}
    if container == "mp3":
        event.update(action="truncate_file", boundary="mp3_after_16_frames")
    else:
        event.update(action="corrupt_container_header",
                     region="mp4_moov" if container == "mp4" else "mkv_cues")
    raw["timeline"] = [event] if damaged else []
    path = directory / ("damaged.json" if damaged else "healthy.json")
    path.write_text(json.dumps(raw))
    out = directory / ("damaged" if damaged else "healthy")
    materialize_scenario(path, out)
    report = json.loads((out / "materialization.json").read_text())
    assert report["outcome"] == "success"
    return out / report["materialized"][0]["location_path"], report


def mp4_boxes(data):
    boxes = {}
    offset = 0
    while offset < len(data):
        size = int.from_bytes(data[offset:offset + 4], "big")
        assert 8 <= size <= len(data) - offset
        boxes[data[offset + 4:offset + 8]] = offset + 4
        offset += size
    assert offset == len(data)
    return boxes


def generate(directory):
    directory.mkdir(mode=0o700)
    library = directory / "library"
    library.mkdir()
    receipts = []
    for index, (name, container, placement) in enumerate(CELLS):
        cell = directory / name
        cell.mkdir()
        healthy, _ = scenario(cell, container, placement, False)
        damaged, report = scenario(cell, container, placement, True)
        assert healthy.stat().st_size <= 64 * 1024 * 1024
        assert damaged.stat().st_size <= 64 * 1024 * 1024
        original, output = healthy.read_bytes(), damaged.read_bytes()
        assert len(report["corruption_actions"]) == 1
        action = report["corruption_actions"][0]
        # Hash identity proves this original is the mutation's actual preimage;
        # independent muxes are never assumed byte-identical.
        for field, data in [("input", original), ("output", output)]:
            assert action[f"{field}_content_hash"] == "sha256:" + hashlib.sha256(data).hexdigest()
            assert action[f"{field}_size_bytes"] == len(data)
        assert original != output
        healthy_decode = tool(["ffmpeg", "-v", "error", "-nostdin", "-i", str(healthy),
                               "-f", "framemd5", "-"])
        assert healthy_decode.invocation.exit_code == 0, healthy_decode.stderr_tail
        assert any(
            line and not line.startswith(b"#")
            for line in healthy_decode.stdout.splitlines()
        )
        probe = tool(["ffprobe", "-v", "error", "-show_format", "-of", "json", str(damaged)])
        if container == "mp4":
            boxes = mp4_boxes(original)
            offset = boxes[b"moov"]
            assert (offset < boxes[b"mdat"]) == (placement == "moov_at_start")
            assert probe.invocation.exit_code != 0
            assert action["probe_outcome"] == "failed_expected"
        elif container == "mkv":
            info = tool(["mkvinfo", "--ui-language", "en_US", "--all", "--positions", str(healthy)])
            assert info.invocation.exit_code == 0, info.stderr_tail
            match = re.search(r"\+ Cues at (\d+)", info.stdout_text())
            assert match is not None, info.stdout_text()
            offset = int(match[1])
            assert original[offset:offset + 4] == bytes.fromhex("1c53bb6b")
            decoded = tool([
                "ffmpeg", "-v", "error", "-nostdin", "-i", str(damaged),
                "-f", "framemd5", "-",
            ])
            assert decoded.invocation.exit_code == 0, decoded.stderr_tail
            assert decoded.stdout == healthy_decode.stdout
        else:
            full = packets(healthy)
            offset = int(full[15]["pos"]) + int(full[15]["size"])
            assert len(full) > 16 and len(packets(damaged)) == 16
            assert output == original[:offset]
            pcm = [tool(["ffmpeg", "-v", "error", "-nostdin", "-i", str(path), "-f", "s16le", "-"])
                   for path in (healthy, damaged)]
            assert all(result.invocation.exit_code == 0 for result in pcm)
            assert 0 < len(pcm[1].stdout) < len(pcm[0].stdout)
        if container != "mp3":
            assert output == original[:offset] + bytes(4) + original[offset + 4:]
        assert (action["byte_start"], action["byte_count"]) == (
            offset, len(original) - offset if container == "mp3" else 4)
        if container != "mp4":
            assert probe.invocation.exit_code == 0, probe.stderr_tail
            assert action["probe_outcome"] == "still_probeable"
        # Failures sort first, so continued healthy observations demonstrate isolation.
        shutil.copyfile(damaged, library / f"0{index}-{name}.{container}")
        if placement != "moov_at_start":
            shutil.copyfile(healthy, library / f"10-healthy.{container}")
        receipts.append(action)
    (directory / "structural-evidence.json").write_text(json.dumps(receipts))
    assert len(list(library.iterdir())) == 7
    return {"cells": [cell[0] for cell in CELLS], "files": 7}


if __name__ == "__main__":
    print(json.dumps(generate(Path(sys.argv[1]).resolve())))
