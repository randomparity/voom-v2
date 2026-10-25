"""Explicit pinned corpus; fixture expectations never populate VOOM observations."""

import copy
import json
import stat
import sys
from pathlib import Path
from unittest.mock import patch

from ruamel.yaml import YAML

from chaos_librarian.adapter.fixture import load_fixture
from chaos_librarian.adapter.probe import compare_probed_media
from chaos_librarian.contract.observed_state import ObservedState
from chaos_librarian.contract.profiles import CANONICAL_FUZZ_LANES, FuzzLaneName, FuzzProfileName
from chaos_librarian.contract.scenario import Scenario
from chaos_librarian.generation import generate_scenario, plan_generation_batch
from chaos_librarian.generation.lanes import coverage_for_payload
from chaos_librarian.materializer.replay import replay_run_bundle
from chaos_librarian.materializer.runtime import wall_clock
from chaos_librarian.topology import iter_asset_contexts

PROFILE = FuzzProfileName.FUZZ_REGRESSION
CELLS = {"adversarial:" + cell for cell in (
    "flags:multiple", "flags:none", "flags:default_forced", "flags:forced_only",
    "metadata:title", "metadata:year", "metadata:long", "metadata:control",
    "metadata:delete", "metadata:missing", "metadata:languages",
    "structural:mp4-moov_at_start", "structural:mp4-moov_at_end",
    "structural:mkv-cues", "structural:mp3-16-frames",
)}
NETWORK_ACTIONS = ["change_permissions", "simulate_quota_exceeded", "toggle_readonly",
                   "simulate_stale_handle", "unmount_path", "remount_path",
                   "acquire_lock", "release_lock"]
FAILED = {"adversarial": {"damage_mp4_start", "damage_mp4_end"},
          "replay": {"damage_mp4_start", "damage_mp4_end"},
          "corrupt-tags": {"asset_005"}, "network": set(), "ass": set()}


def assert_cells(payload):
    actual = coverage_for_payload(payload).cells
    assert CELLS <= actual, sorted(CELLS - actual)


def timeline_summary(bundle):
    timeline = YAML(typ="safe").load(bundle["scenario"])["timeline"]
    applied = bundle["applied_events"]
    assert isinstance(applied, int) and 0 <= applied <= len(timeline)
    return {"applied_events": applied, "timeline_events": len(timeline),
            "timeline_complete": applied == len(timeline)}


def assert_events(fixture, count):
    timeline = YAML(typ="safe").load(fixture.replay_bundle.scenario)["timeline"]
    assert len(timeline) == count
    bundle = fixture.replay_bundle.model_dump(mode="json")
    assert timeline_summary(bundle)["timeline_complete"], "incomplete timeline prefix"
    for prefix in (0, count - 1):
        if prefix < count:
            assert not timeline_summary({**bundle, "applied_events": prefix})["timeline_complete"]
    assert [e.event_id for e in fixture.journal] == [e["id"] for e in timeline]


def generated(lane):
    return generate_scenario(PROFILE, 248, lane=lane)


def run(path, out):
    wall_clock.run_wall_clock_scenario(path, out, duration="30s", speed="1000x")
    return load_fixture(out)


def write_slice(directory, lane, actions, keep):
    source = generated(lane)
    payload = YAML(typ="safe").load(source.yaml_bytes)
    payload["timeline"] = [e for e in payload["timeline"] if e["action"] in actions]
    # Exact selected prefix, including both close actions, rather than random tail events.
    if lane == FuzzLaneName.NETWORK_FS_CHAOS:
        payload["timeline"] = payload["timeline"][:8]
    payload["movies"] = [movie for movie in payload["movies"]
                         if movie["variants"][0]["bundle"]["assets"][0]["id"] in keep]
    for movie in payload["movies"]:
        for variant in movie["variants"]:
            for asset in variant["bundle"]["assets"]:
                asset["duration_seconds"] = 0.2
                if asset.get("video"):
                    asset["video"]["resolution"] = "sd"
    model = Scenario.model_validate(payload)
    path = directory / f"{lane.value}.json"
    path.write_text(model.model_dump_json(by_alias=True, exclude_none=True))
    # Validate the serialized slice too; preserve the unmodified generator source as provenance.
    Scenario.model_validate_json(path.read_text())
    from chaos_librarian.validation import prepare_run_input_from_bytes, run_validation
    prepared = prepare_run_input_from_bytes(raw_bytes=path.read_bytes(), source_label=str(path))
    assert run_validation(prepared).ok
    (directory / f"{lane.value}-generated.yaml").write_bytes(source.yaml_bytes)
    return path, model


def generate(directory):
    directory.mkdir()
    original = generated(FuzzLaneName.ADVERSARIAL_MEDIA)
    assert original.yaml_bytes == Path(
        "tests/fixtures/scenarios/fuzz-adversarial-seed-248.yaml"
    ).read_bytes()
    payload = YAML(typ="safe").load(original.yaml_bytes)
    assert_cells(payload)
    assets = [c.asset for c in iter_asset_contexts(original.scenario)]
    assert len(assets) == 18 and len(original.scenario.timeline) == 11
    assert max(a.duration_seconds for a in assets) == 2
    assert max(len(a.audio) for a in assets) == max(len(a.subtitles) for a in assets) == 8
    default = CANONICAL_FUZZ_LANES[PROFILE]
    assert len(default) == 10 and FuzzLaneName.ADVERSARIAL_MEDIA not in default
    assert [item.lane for item in plan_generation_batch(PROFILE, 248, 20)] == list(default) * 2
    # Removing recipe parameters must lose actual coverage, even with action names intact.
    for target in ("metadata_year", "damage_mp3"):
        changed = copy.deepcopy(payload)
        for event in changed["timeline"]:
            if event.get("target") == target:
                if target == "metadata_year":
                    event["fields"] = {"title": "ordinary"}
                else:
                    event.pop("boundary")
                    event["keep_bytes"] = 4096
        try:
            assert_cells(changed)
        except AssertionError:
            pass
        else:
            raise AssertionError("recipe mutation falsely retained coverage")
    path = directory / "adversarial.yaml"
    path.write_bytes(original.yaml_bytes)
    fixture = run(path, directory / "adversarial")
    assert_events(fixture, 11)
    replay_run_bundle(fixture.replay_bundle, directory / "replay")
    assert_events(load_fixture(directory / "replay"), 11)
    from xml.etree import ElementTree
    for name in ("adversarial", "replay"):
        nfo = directory / name / "library/movies/Disagreement.nfo"
        assert ElementTree.parse(nfo).findtext("title") == "Sidecar title"
    path, model = write_slice(directory, FuzzLaneName.MALFORMED, {"corrupt_tags"},
                              {"asset_005", "asset_007"})
    assert len(model.timeline) == 1 and model.timeline[0].id == "fuzz_0005_corrupt_tags"
    tags = run(path, directory / "corrupt-tags")
    assert_events(tags, 1)
    report = json.loads((directory / "corrupt-tags/materialization.json").read_text())
    action, = report["corruption_actions"]
    assert action["action"] == "corrupt_tags" and action["byte_count"] == 64
    assert action["input_content_hash"] != action["output_content_hash"]
    assert action["probe_outcome"] == "failed_expected"
    path, model = write_slice(directory, FuzzLaneName.NETWORK_FS_CHAOS,
                              set(NETWORK_ACTIONS), {f"asset_{n:03}" for n in range(1, 9)})
    assert [e.action for e in model.timeline] == NETWORK_ACTIONS
    assert model.timeline[5].for_ == model.timeline[4].id
    assert model.timeline[7].for_ == model.timeline[6].id
    modes = []
    restore = wall_clock.restore_chaos_modes

    def observe_restore(state):
        original = {str(p.relative_to(state.library_root)): mode
                    for p, mode in state.captured_modes.items()}
        before = {str(p.relative_to(state.library_root)): stat.S_IMODE(p.stat().st_mode)
                  for p in state.captured_modes}
        assert len(before) == 2 and all(mode & 0o222 == 0 for mode in before.values())
        restore(state)
        after = {p: stat.S_IMODE((state.library_root / p).stat().st_mode) for p in before}
        assert after == original and all(mode & 0o200 for mode in after.values())
        modes.append({"before": before, "after": after})

    with patch.object(wall_clock, "restore_chaos_modes", observe_restore):
        network = run(path, directory / "network")
    assert_events(network, 8)
    report = json.loads((directory / "network/materialization.json").read_text())
    receipts = report["network_fs_chaos_actions"]
    assert [a["action"] for a in receipts] == [*NETWORK_ACTIONS[:4],
                                               "remount_path", "release_lock"]
    assert [a["condition"] for a in receipts] == ["eacces", "enospc", "eacces",
                                                  "estale", "unavailable", "eagain"]
    assert [a["enforced"] for a in receipts] == [True, False, True, False, False, False]
    for action, index in zip(receipts[-2:], (4, 6), strict=True):
        assert action["event_id"] == model.timeline[index].id
        assert action["related_event_id"] == model.timeline[index + 1].id
    assert len(modes) == 1
    (directory / "network-modes.json").write_text(json.dumps(modes))
    # Reuse #743's supported standalone ASS recipe without touching the golden corpus.
    import track_cases
    ass_path = directory / "ass.json"
    import contextlib
    import io
    with contextlib.redirect_stdout(io.StringIO()):
        track_cases.generate(ass_path)
    ass_payload = json.loads(ass_path.read_text())
    ass_payload["movies"][0]["variants"] = ass_payload["movies"][0]["variants"][-1:]
    ass_path.write_text(Scenario.model_validate(ass_payload).model_dump_json())
    from chaos_librarian.materializer import materialize_scenario
    materialize_scenario(ass_path, directory / "ass")
    return {"seed": 248, "cells": sorted(CELLS), "runs": list(FAILED),
            "supplemental": {"corrupt-tags": 1, "network": 8, "network_receipts": 6},
            "network_consumer_phase": "recovered-only"}


def require_evidence(raw):
    assert isinstance(raw.get("format_tags"), dict), "missing/null format tags"
    assert isinstance(raw.get("streams"), list) and raw["streams"], "missing streams"
    for stream in raw["streams"]:
        if stream["kind"] in ("audio", "subtitle"):
            # MP3 recipes have no language-tag evidence in the pinned probe contract.
            if raw["container"] != "mp3":
                assert isinstance(stream.get("language"), str), "missing language"
            assert isinstance(stream.get("default"), bool), "missing default"
            assert isinstance(stream.get("forced"), bool), "missing forced"


def compare(directory, name):
    run_dir = directory / name
    fixture = load_fixture(run_dir)
    raw = json.loads((run_dir / "observed-state.json").read_text())
    observed = ObservedState.model_validate(raw)
    assert observed.run_id == fixture.run_id
    by_path = {a.current_path: a for a in observed.assets}
    assert len(by_path) == len(observed.assets)
    semantic = {}
    for asset_id, report in fixture.reports.assets.items():
        current = report.current
        assert current is not None
        if asset_id in FAILED[name]:
            assert current.location_path not in by_path, "failed probe invented an asset"
            continue
        actual = by_path.pop(current.location_path)
        require_evidence(actual.probed.model_dump())
        assert current.probed is not None
        differences = compare_probed_media(current.probed, actual.probed)
        assert differences == [], (asset_id, differences)
        facts = actual.probed.model_dump(mode="json")
        semantic[asset_id] = facts
        # Both missing and explicit null evidence must fail despite the permissive oracle.
        missing_cases = [{k: v for k, v in facts.items() if k != "format_tags"},
                         {**facts, "format_tags": None}]
        for field in ("default", "forced", "language"):
            if field == "language" and facts["container"] == "mp3":
                continue
            for null in (False, True):
                missing = copy.deepcopy(facts)
                stream = next(s for s in missing["streams"] if s["kind"] == "audio")
                stream.pop(field)
                if null:
                    stream[field] = None
                missing_cases.append(missing)
        for missing in missing_cases:
            try:
                require_evidence(missing)
            except AssertionError:
                pass
            else:
                raise AssertionError("missing metadata falsely accepted")
    assert not by_path, sorted(by_path)
    if name == "ass":
        sidecars = observed.assets[0].sidecars
        assert len(sidecars) == 1 and sidecars[0].path.endswith(".fra.ass")
        assert (run_dir / "library" / sidecars[0].path).is_file()
    if name == "replay":
        original = json.loads((directory / "adversarial-semantic.json").read_text())
        # Numeric durations tolerate the same bounded probe differences as the upstream oracle.
        assert semantic.keys() == original.keys()
        from chaos_librarian.contract.manifest import ProbedMedia
        for asset_id, facts in semantic.items():
            assert compare_probed_media(ProbedMedia(**original[asset_id]), ProbedMedia(**facts)) == []
    (directory / f"{name}-semantic.json").write_text(json.dumps(semantic))
    return {"semantic_assets": len(semantic), "failed_assets": sorted(FAILED[name])}


if __name__ == "__main__":
    mode, directory, *rest = sys.argv[1:]
    if mode == "summary":
        result = timeline_summary(json.loads((Path(directory) / "replay.json").read_text()))
    else:
        result = generate(Path(directory)) if mode == "generate" else compare(Path(directory), rest[0])
    print(json.dumps(result))
