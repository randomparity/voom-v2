"""Pinned upstream retained-metadata cases; observations come only from VOOM's DB."""

import json
import sys
from pathlib import Path
from xml.etree import ElementTree

from ruamel.yaml import YAML

from chaos_librarian.adapter.fixture import load_fixture
from chaos_librarian.adapter.probe import compare_probed_media
from chaos_librarian.contract.observed_state import ObservedState

# Mirrors tests/integration/test_misleading_metadata_real.py at the pinned SHA.
CASES = {
    "embedded-title": {"title": "Embedded title"},
    "extreme-year": {"year": "9999"},
    "unicode-boundary": {"title": "雪" * 1365 + "x"},
    "control-characters": {"title": "雪 café\tline\nnext"},
    "deleted-tags": {"title": "", "year": ""},
    "combined-tags": {"title": "Embedded title", "year": "9999"},
    "missing-tags-languages": None,
}


def generate(directory):
    scenarios = []
    for name, fields in CASES.items():
        payload = YAML(typ="safe").load(
            Path("tests/fixtures/scenarios/edit-metadata.yaml").read_text()
        )
        payload["seed"] = 246
        asset = payload["movies"][0]["variants"][0]["bundle"]["assets"][0]
        asset["video"]["resolution"] = "sd"
        if fields is None:
            payload["timeline"] = []
            asset["audio"].append(
                {"codec": "aac", "channels": "stereo", "language": "fra"}
            )
        else:
            payload["timeline"][0]["fields"] = fields
            payload["timeline"].append(
                {
                    "id": "nfo",
                    "at": "2s",
                    "action": "create_sidecar",
                    "target": "a0",
                    "kind": "nfo",
                    "to": "movies/T.nfo",
                    "body": "<movie><title>Sidecar title</title></movie>",
                }
            )
            if fields.get("title") == "":
                payload["timeline"].insert(
                    0,
                    {
                        "id": "prior",
                        "at": "0s",
                        "action": "edit_metadata",
                        "target": "a0",
                        "fields": {"title": "Title to delete", "year": "2026"},
                    },
                )
        path = directory / f"{name}.json"
        path.write_text(json.dumps(payload))
        scenarios.append(str(path))
    return scenarios


def require_metadata(probed, tags, languages):
    assert probed.get("format_tags") == tags, "retained format-tag evidence differs"
    audio = [stream for stream in probed["streams"] if stream["kind"] == "audio"]
    assert [stream.get("language") for stream in audio] == languages, (
        "audio label evidence differs"
    )


def compare(run, observed_path):
    fixture = load_fixture(run)
    scenario = YAML(typ="safe").load(fixture.replay_bundle.scenario)
    edits = [
        event for event in scenario["timeline"] if event["action"] == "edit_metadata"
    ]
    fields = edits[-1]["fields"] if edits else {}
    tags = {key: value for key, value in fields.items() if value != ""}
    languages = ["eng"] if edits else ["eng", "fra"]
    journal = [
        json.loads(line) for line in (run / "journal.jsonl").read_text().splitlines()
    ]
    for edit in edits:
        assert (
            next(e for e in journal if e["event_id"] == edit["id"])["state_delta"][
                "fields"
            ]
            == edit["fields"]
        )
    media = list((run / "library").rglob("*.mkv"))
    assert len(media) == 1 and media[0].stat().st_size <= 64 * 1024 * 1024
    if edits:
        assert (
            ElementTree.parse(run / "library/movies/T.nfo").findtext("title")
            == "Sidecar title"
        )
        if tags.get("title"):
            assert (
                tags["title"] not in media[0].name and tags["title"] != "Sidecar title"
            )
    observed = ObservedState.model_validate_json(observed_path.read_text())
    assert observed.run_id == fixture.run_id
    assets = [asset for asset in observed.assets if asset.current_path.endswith(".mkv")]
    assert len(assets) == 1 and assets[0].probed is not None
    actual = assets[0].probed
    require_metadata(actual.model_dump(), tags, languages)
    expected = max(
        fixture.current_manifest.versions, key=lambda version: version.index
    ).probed
    assert expected is not None and 0 < expected.duration_seconds <= 30
    assert expected.format_tags == tags
    assert compare_probed_media(expected, actual) == []
    # Missing evidence must fail coverage before the oracle's permissive null comparison.
    for field in ("absent-tags", "null-tags", "absent-language"):
        missing = actual.model_dump()
        if field == "absent-tags":
            del missing["format_tags"]
        elif field == "null-tags":
            missing["format_tags"] = None
        else:
            next(s for s in missing["streams"] if s["kind"] == "audio").pop("language")
        try:
            require_metadata(missing, tags, languages)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"missing evidence accepted: {field}")
    for key in ("title", "year"):
        altered = actual.model_copy(deep=True)
        altered.format_tags[key] = "different"
        differences = compare_probed_media(expected, altered)
        assert (f"format_tags.{key}", tags.get(key), "different") in differences
    altered = actual.model_copy(deep=True)
    index = next(
        i for i, stream in enumerate(altered.streams) if stream.kind.value == "audio"
    )
    altered.streams[index].language = "zzz"
    assert (f"streams.{index}.language", "eng", "zzz") in compare_probed_media(
        expected, altered
    )
    return {"metadata_compared": True, "format_tags": tags, "audio_labels": languages}


if __name__ == "__main__":
    mode, source, output = sys.argv[1:]
    result = (
        generate(Path(source))
        if mode == "generate"
        else compare(Path(source), Path(output))
    )
    print(json.dumps(result))
