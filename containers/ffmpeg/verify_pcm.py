#!/usr/bin/env python3
"""Prove the patched muxer's observed facts, headers, samples and worker profile."""
import argparse
import json
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import time
import uuid

LAYOUTS = [("mono", 1, 4), ("stereo", 2, 3), ("5.1", 6, 63),
           ("5.1(side)", 6, 1551), ("7.1", 8, 1599)]
PCM_GUID = bytes.fromhex("0100000000001000800000aa00389b71")


def command(argv, data=None):
    result = subprocess.run(argv, input=data, capture_output=True, timeout=60,
                            env={**os.environ, "VOOM_MEDIA_CALLER_PID": str(os.getpid())})
    if result.returncode:
        raise RuntimeError(f"{argv[0]} exited {result.returncode}: "
                           f"{result.stderr.decode(errors='replace')}")
    return result.stdout


class Tools:
    def __init__(self, args):
        self.args = args

    def run(self, tool, args, data=None, directory=None):
        executable = getattr(self.args, tool)
        if self.args.engine:
            name = "voom-pcm-proof-" + uuid.uuid4().hex
            permissions = (["--user", f"{os.getuid()}:{os.getgid()}"]
                           if self.args.engine == "docker" else ["--userns=keep-id:uid=65532,gid=65532", "--user", "65532:65532"])
            mounts = (["--mount", f"type=bind,source={directory},target={directory}"]
                      if directory else [])
            prefix = [self.args.engine, "run", "--name", name, "--label",
                      "io.voom.proof=" + name, "--rm", "--pull=never", "--network=none",
                      "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges",
                      "-i"] + permissions + mounts + ["--entrypoint", f"/usr/local/bin/{tool}",
                                                     self.args.image]
        else:
            prefix = [executable]
        try:
            return command(prefix + args, data)
        except BaseException:
            if self.args.engine:
                observed = subprocess.run([self.args.engine, "inspect", name],
                                          capture_output=True, timeout=5)
                if observed.returncode == 0:
                    record = json.loads(observed.stdout)[0]
                    if record["Config"]["Labels"].get("io.voom.proof") == name:
                        command([self.args.engine, "rm", "-f", record["Id"]])
            raise

    def probe(self, data):
        return json.loads(self.run("ffprobe", ["-v", "error", "-show_streams", "-of",
                                                "json", "pipe:0"], data))["streams"][0]

    def decode(self, data):
        return self.run("ffmpeg", ["-v", "error", "-guess_layout_max", "0", "-i",
                                   "pipe:0", "-map", "0:a:0", "-c:a", "pcm_s32le",
                                   "-dither_method", "0", "-f", "s32le", "pipe:1"], data)


def samples(bits, channels):
    extrema = [-(1 << (bits - 1)), (1 << (bits - 1)) - 1, -1, 1, 257, -257, 19, -31]
    values = [extrema[(frame + channel) % len(extrema)]
              for frame in range(64) for channel in range(channels)]
    raw = b"".join(value.to_bytes(bits // 8, "little", signed=True) for value in values)
    expected = b"".join(struct.pack("<i", value << (32 - bits)) for value in values)
    return raw, expected


def check_facts(tools, data, bits, rate, layout, channels, expected):
    facts = tools.probe(data)
    observed_bits = int(facts.get("bits_per_raw_sample") or facts["bits_per_sample"])
    assert (facts["codec_name"], int(facts["sample_rate"]), facts["channel_layout"],
            facts["channels"], observed_bits) == (f"pcm_s{bits}le", rate, layout, channels, bits), facts
    assert tools.decode(data) == expected, "decoded integer samples changed"


def check_pcm(tools, directory):
    count = 0
    for bits in (16, 24):
        for layout, channels, mask in LAYOUTS:
            for rate in (44100, 96000):
                raw, expected = samples(bits, channels)
                args = ["-v", "error", "-f", f"s{bits}le", "-ar", str(rate),
                        "-channel_layout", layout, "-i", "pipe:0", "-c:a", f"pcm_s{bits}le",
                        "-channel_layout", layout, "-f", "matroska"]
                data = tools.run("ffmpeg", args + ["-pcm_wavex", "1", "pipe:1"], raw)
                check_facts(tools, data, bits, rate, layout, channels, expected)
                source = directory / "input.mkv"
                source.write_bytes(data)
                track = json.loads(command([tools.args.mkvmerge, "-J", str(source)]))["tracks"][0]
                properties = track["properties"]
                private = bytes.fromhex(properties["codec_private_data"])
                header = struct.pack("<HHIIHHHHI", 65534, channels, rate,
                                     rate * channels * bits // 8, channels * bits // 8,
                                     bits, 22, bits, mask) + PCM_GUID
                assert properties["codec_id"] == "A_MS/ACM", properties
                assert private == header, (private.hex(), header.hex())
                remux = directory / "remux.mkv"
                command([tools.args.mkvmerge, "-o", str(remux), str(source)])
                check_facts(tools, remux.read_bytes(), bits, rate, layout, channels, expected)
                remux.unlink()
                native = tools.run("ffmpeg", args + ["pipe:1"], raw)
                assert "channel_layout" not in tools.probe(native), "default mux behavior changed"
                assert tools.decode(native) == expected
                count += 1
    return count


def check_packet_timing(tools, directory):
    source = directory / "timing-source.mkv"
    tools.run("ffmpeg", ["-v", "error", "-f", "lavfi", "-i", "sine=sample_rate=48000",
                         "-t", "0.1", "-c:a", "flac", str(source)], directory=directory)
    packets = []
    for name, options in (("source", None), ("native", []), ("preserved", ["-pcm_wavex", "1"])):
        path = source if options is None else directory / ("timing-" + name + ".mkv")
        if options is not None:
            tools.run("ffmpeg", ["-v", "error", "-i", str(source), "-c", "copy"] +
                      options + [str(path)], directory=directory)
        observed = json.loads(tools.run("ffprobe", ["-v", "error", "-show_packets",
                              "-show_data_hash", "sha256", "-of", "json", "pipe:0"],
                              path.read_bytes()))["packets"]
        packets.append([[packet.get(key) for key in ("data_hash", "pts", "dts", "duration")]
                        for packet in observed])
    assert packets[0][-1][-1] == 15 and packets[1][-1][-1] == 14, "native timing control changed"
    assert packets[0] == packets[2], "opt-in mux lost copied audio packet timing"


def check_profile(tools, directory):
    for encoder in ("libx264", "libx265", "libsvtav1", "libaom-av1"):
        tools.run("ffmpeg", ["-v", "error", "-f", "lavfi", "-i",
                             "testsrc=size=64x64:rate=10", "-frames:v", "1",
                             "-pix_fmt", "yuv420p", "-c:v", encoder, "-threads", "2",
                             "-f", "matroska", str(directory / "profile.mkv")],
                  directory=directory)
        assert tools.probe((directory / "profile.mkv").read_bytes())["codec_type"] == "video"
        (directory / "profile.mkv").unlink()
    for encoder, container in (("aac", "mp4"), ("libopus", "ogg"), ("ac3", "matroska"),
                               ("eac3", "matroska"), ("flac", "matroska"), ("alac", "matroska")):
        args = ["-v", "error", "-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo",
                "-t", "0.05", "-c:a", encoder]
        if container == "mp4":
            args += ["-movflags", "frag_keyframe+empty_moov"]
        tools.run("ffmpeg", args + ["-f", container, "pipe:1"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ffmpeg", default="ffmpeg")
    parser.add_argument("--ffprobe", default="ffprobe")
    parser.add_argument("--mkvmerge", default="mkvmerge")
    parser.add_argument("--engine", choices=("docker", "podman"))
    parser.add_argument("--image")
    args = parser.parse_args()
    if bool(args.engine) != bool(args.image):
        parser.error("--engine and --image must be supplied together")
    tools = Tools(args)
    started = time.monotonic()
    help_text = tools.run("ffmpeg", ["-hide_banner", "-h", "muxer=matroska"])
    assert any(line.split()[:2] == [b"-pcm_wavex", b"<boolean>"]
               for line in help_text.splitlines()), "required pcm_wavex mux capability is absent"
    root = Path(__file__).resolve().parents[2] / ".test-tmp"
    root.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="pcm-mux-", dir=root) as temporary:
        check_packet_timing(tools, Path(temporary))
        count = check_pcm(tools, Path(temporary))
        check_profile(tools, Path(temporary))
    print(json.dumps({"pcm_cells": count, "remux_cells": count, "native_controls": count,
                      "profile_encoders": 10, "copied_timing_cases": 2,
                      "seconds": round(time.monotonic() - started, 3)}))


if __name__ == "__main__":
    main()
