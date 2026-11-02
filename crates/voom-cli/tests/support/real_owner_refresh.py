"""Refresh a legacy cover snapshot through the real authenticated owner stack."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import time


def run(arguments):
    result = subprocess.run(list(map(str, arguments)), capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise RuntimeError(f"{arguments[0]} failed ({result.returncode}): {result.stderr}")
    return result.stdout


def main():
    os.umask(0o077)
    repo = Path(__file__).resolve().parents[4]
    spec = importlib.util.spec_from_file_location("bootstrap", repo / "scripts/acceptance-bootstrap.py")
    bootstrap = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bootstrap)
    args = bootstrap.parser().parse_args()
    source = args.input[0] / "cover.mp4"
    cover = args.workdir.parent / "cover.png"
    run(["ffmpeg", "-nostdin", "-v", "error", "-f", "lavfi", "-i", "color=size=32x32",
         "-frames:v", "1", "-threads", "1", cover])
    run(["ffmpeg", "-nostdin", "-v", "error", "-f", "lavfi", "-i",
         "color=size=64x64:rate=5:duration=0.4", "-f", "lavfi", "-i", "sine=duration=0.4",
         "-i", cover, "-map", "0:v", "-map", "1:a", "-map", "2:v", "-c:v:0", "libx264",
         "-threads:v:0", "1", "-c:a", "aac", "-c:v:1", "copy", "-disposition:v:1",
         "attached_pic", source])
    source_identity = (source.stat().st_size, source.stat().st_mtime_ns,
                       hashlib.sha256(source.read_bytes()).hexdigest())
    policy_path = args.workdir.parent / "refresh.voom"
    policy_path.write_text('policy "snapshot refresh" { phase remux { container mkv } }\n')
    with bootstrap.Bootstrap(args) as stack:
        root = next(root for root in stack.roots if root["role"] == "input")
        policy = stack.voom("policy", "create", "--slug", "snapshot-refresh", "--file", policy_path)["data"]
        version_id = policy["version"]["version_id"]
        with sqlite3.connect(stack.workdir / "voom.db") as db:
            for attempt in range(2):
                scanned = stack.voom("scan", "--root", root["root_id"])["data"]
                assert scanned["status"] == "succeeded" and scanned["observation_count"] == 1, scanned
                deadline = time.monotonic() + 30
                while True:
                    settled = db.execute(
                        "SELECT state FROM tickets WHERE kind = 'synthetic.workflow.operation.scan_library' "
                        "AND CAST(json_extract(payload, '$.rendered_payload.scan_session_id') AS INTEGER) = ?",
                        (scanned["scan_session_id"],),
                    ).fetchall()
                    if settled == [("succeeded",)]:
                        break
                    if time.monotonic() >= deadline:
                        raise RuntimeError(f"scan {scanned['scan_session_id']} did not settle: {settled}")
                    time.sleep(0.05)
                if attempt == 0:
                    snapshot_id, file_version, payload = db.execute(
                        "SELECT id, file_version_id, payload FROM media_snapshots"
                    ).fetchone()
                    legacy = json.loads(payload)
                    pictures = [stream for stream in legacy["streams"]
                                if stream.get("disposition", {}).get("attached_pic")]
                    assert len(pictures) == 1, legacy
                    assert not pictures[0].get("filename") and not pictures[0].get("mime_type"), pictures
                    pictures[0]["disposition"].pop("attached_pic")
                    # Fixture setup only: emulate an old provider in this fresh DB,
                    # before capturing historical inputs or asking for a refresh.
                    old_payload = json.dumps(legacy, separators=(",", ":"))
                    db.execute("UPDATE media_snapshots SET payload = ? WHERE id = ?", (old_payload, snapshot_id))
                    db.commit()
                    original_version = db.execute(
                        "SELECT id, content_hash, size_bytes, epoch FROM file_versions WHERE id = ?",
                        (file_version,),
                    ).fetchone()
                    old_input = stack.voom("policy", "input", "create-from-scan", "--root", root["root_id"],
                                           "--slug", "legacy-cover")["data"]["input_set"]["input_set_id"]
                    original_member = db.execute(
                        "SELECT existing_media_snapshot_id, stream_summary FROM policy_media_snapshot_inputs "
                        "WHERE policy_input_set_id = ?", (old_input,),
                    ).fetchone()
                    assert original_member[0] == snapshot_id
                    assert json.loads(original_member[1])["video_stream_count"] == 2
                    old_plan = stack.voom("plan", "show", "--policy-version-id", version_id,
                                          "--input-set-id", old_input)["data"]["plan"]
                    assert old_plan["nodes"][0]["operation_payload"]["source_media_snapshot_id"] == snapshot_id
            history = db.execute(
                "SELECT id, file_version_id, payload FROM media_snapshots ORDER BY id"
            ).fetchall()
            assert len(history) == 2, f"unchanged rescan discarded fresh probe facts: {history}"
            assert history[0] == (snapshot_id, file_version, old_payload), "refresh rewrote history"
            refreshed_id = history[1][0]
            assert refreshed_id != snapshot_id and history[1][1] == file_version
            refreshed = json.loads(history[1][2])
            assert sum(bool(stream.get("disposition", {}).get("attached_pic"))
                       for stream in refreshed["streams"]) == 1, refreshed
            assert db.execute("SELECT COUNT(*) FROM file_versions").fetchone()[0] == 1
            assert db.execute(
                "SELECT existing_media_snapshot_id, stream_summary FROM policy_media_snapshot_inputs "
                "WHERE policy_input_set_id = ?", (old_input,),
            ).fetchone() == original_member
            regenerated = stack.voom("plan", "show", "--policy-version-id", version_id,
                                      "--input-set-id", old_input)["data"]["plan"]
            assert regenerated["nodes"][0]["operation_payload"]["source_media_snapshot_id"] == refreshed_id
            for index, selection in enumerate([("--root", root["root_id"]), ("--all",)]):
                inputs = stack.voom("policy", "input", "create-from-scan", *selection,
                                    "--slug", f"refreshed-{index}")["data"]["input_set"]
                assert inputs["included_count"] == 1
                member = db.execute(
                    "SELECT existing_media_snapshot_id, stream_summary FROM policy_media_snapshot_inputs "
                    "WHERE policy_input_set_id = ?", (inputs["input_set_id"],),
                ).fetchone()
                assert member[0] == refreshed_id and json.loads(member[1])["video_stream_count"] == 1
            stage = next(root for root in stack.roots if root["role"] == "staging")
            output = next(root for root in stack.roots if root["role"] == "output")
            execution = stack.voom(
                "compliance", "execute", "--policy-version-id", version_id,
                "--input-set-id", inputs["input_set_id"], "--staging-root", stage["provider_locator"],
                "--output-dir", output["provider_locator"],
            )["data"]
            assert execution["summary"]["per_operation"]["remux"]["success_count"] == 1, execution
            assert execution["summary"]["per_operation"]["remux"]["failure_count"] == 0, execution
            assert execution["file_phases"][0]["outcome"] == "committed", execution
            produced = list(Path(output["provider_locator"]).rglob("*.mkv"))
            assert len(produced) == 1, produced
            identified = json.loads(run(["mkvmerge", "-J", produced[0]]))
            assert len(identified["attachments"]) == 1, identified
            assert db.execute(
                "SELECT id, content_hash, size_bytes, epoch FROM file_versions WHERE id = ?",
                (file_version,),
            ).fetchone() == original_version
            assert db.execute("SELECT payload FROM media_snapshots WHERE id = ?", (snapshot_id,)).fetchone()[0] == old_payload
    assert (source.stat().st_size, source.stat().st_mtime_ns,
            hashlib.sha256(source.read_bytes()).hexdigest()) == source_identity, "source media changed"
    print(json.dumps({"rescans": 2, "source_version_preserved": True, "history_preserved": True,
                      "input_pin_preserved": True, "new_plan_uses_refresh": True,
                      "root_and_whole_input_video_count": 1, "remux_committed": True,
                      "attachments": 1}), flush=True)


if __name__ == "__main__":
    main()
