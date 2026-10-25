"""Author the pinned flag cells and an independent healthy ASS control."""

import json
import sys
from pathlib import Path

from chaos_librarian.generation.adversarial import assets


def generate(path):
    # Only explicitly authored flag assets; no metadata or structural mutations.
    flags = assets()[:4]
    assert [asset["id"] for asset in flags] == [
        "flags_multiple", "flags_none", "flags_default_forced", "flags_forced_only"
    ]
    control = {
        "id": "healthy", "role": "main", "container": "mkv",
        "duration_seconds": 0.2,
        "video": {"source": "color_bars", "codec": "h264", "resolution": "sd"},
        "audio": [{"source": "sine", "codec": "aac", "channels": "stereo",
                   "language": "eng", "role": "main", "default": True, "forced": False}],
        "subtitles": [
            {"codec": "srt", "language": "eng", "mode": "embedded",
             "default": False, "forced": False},
            {"codec": "ass", "source": "styled_ass", "language": "fra", "mode": "sidecar"},
        ],
    }
    payload = {
        "schema_version": 37, "scenario_id": "voom-track-matrix", "seed": 743,
        "duration_scale": "short", "library": {"roots": [{"id": "main", "path": "movies"}]},
        "movies": [{"id": "tracks", "title": "Tracks", "layout": "movie_flat", "variants": [
            {"id": "variant_" + asset["id"], "label": asset["id"],
             "bundle": {"id": "bundle_" + asset["id"], "assets": [asset]}}
            for asset in [*flags, control]
        ]}],
        "series": [], "artists": [], "timeline": [],
    }
    path.write_text(json.dumps(payload))
    print(json.dumps(str(path)))


if __name__ == "__main__":
    generate(Path(sys.argv[1]))
