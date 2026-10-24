#!/usr/bin/env python3
"""Exercise owner publication and explicit resume with a tiny synthetic source.

The requested new work directory is retained for inspection, including on
failure. The bootstrap owns process shutdown and credential removal.
"""

import argparse
import hashlib
import importlib.util
import json
import os
import sqlite3
import subprocess
import time
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workdir", type=Path, required=True)
    parser.add_argument("--mode", choices=("chain", "extract", "abort"), default="chain")
    parser.add_argument("--apply-delay-seconds", type=float, default=0)
    args = parser.parse_args()
    assert 0 <= args.apply_delay_seconds <= 15, "fixture delay must be bounded"
    repo = Path(__file__).resolve().parents[1]
    run = args.workdir.resolve()
    os.umask(0o077)
    run.mkdir(mode=0o700, parents=True, exist_ok=False)
    for name in ("input", "staging", "output"):
        (run / name).mkdir()
    source = run / "input" / "fixture.mp4"
    encoder = [
        "ffmpeg", "-v", "error", "-f", "lavfi", "-i",
        "testsrc2=size=64x64:rate=10:duration=1", "-f", "lavfi", "-i",
        "anullsrc=channel_layout=5.1:sample_rate=48000", "-t", "1",
        "-c:v", "mpeg4", "-c:a", "aac", "-metadata:s:a:0", "language=eng",
    ]
    if args.mode == "extract":
        encoder += ["-map", "0:v", "-map", "1:a", "-map", "1:a", "-metadata:s:a:1", "language=jpn"]
    subprocess.run(encoder + [str(source)], check=True)
    original = (source.stat().st_mtime_ns, hashlib.sha256(source.read_bytes()).hexdigest())
    policy = (repo / "crates/voom-control-plane/tests/fixtures/policies/reference-user.voom").read_text()
    # This fixture has no subtitle; retain every media mutation in the policy.
    (run / "policy.voom").write_text(policy.replace("    defaults subtitle where forced\n", ""))
    slug = "reference-user-library-normalize"
    if args.mode == "extract":
        (run / "policy.voom").write_text('policy "publication extraction" {\n  phase extract {\n    extract audio\n  }\n}\n')
        slug = "publication-extraction"
    spec = importlib.util.spec_from_file_location("bootstrap", repo / "scripts/acceptance-bootstrap.py")
    bootstrap = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bootstrap)
    configuration = bootstrap.parser().parse_args([
        "serve", "--workdir", str(run / "stack"), "--input", str(run / "input"),
        "--staging", str(run / "staging"), "--output", str(run / "output"),
        "--bin-dir", str(repo / "target/debug"),
    ])
    group_spec = importlib.util.spec_from_file_location("group_fixture", repo / "scripts/issue-733-group-proof-fixture.py")
    group_fixture = importlib.util.module_from_spec(group_spec)
    group_spec.loader.exec_module(group_fixture)

    class CheckedBootstrap(bootstrap.Bootstrap):
        proxy = None

        def configuration(self, credential, base_url):
            self.proof_credential = credential
            if args.mode in ("extract", "chain", "abort"):
                foreign = self.voom("node", "register", "--name", "fixture-other-owner", "--kind", "remote")["data"]["node"]["id"]
                self.proxy = group_fixture.GroupProofProxy(base_url, self.workdir / "voom.db", foreign, self.bin_dir / "voom", self.env, args.mode, args.apply_delay_seconds)
                base_url = self.proxy.url
            configuration = super().configuration(credential, base_url)
            return configuration.replace("lease_ttl_seconds = 30", "lease_ttl_seconds = 5") if args.mode == "extract" else configuration

        def close(self):
            if self.proxy:
                self.proxy.closing = True
            try:
                super().close()
            finally:
                if self.proxy:
                    try:
                        self.proxy.close()
                    finally:
                        (run / "group-proof-checks.json").write_text(json.dumps(self.proxy.checks, indent=2))
                        (run / "group-proof-failures.json").write_text(json.dumps(self.proxy.failures, indent=2))

    def command(stack, name, arguments, expect_error=False):
        started = time.monotonic()
        completed = subprocess.run([
            str(repo / "target/debug/voom"), "--database-url", stack.url, *map(str, arguments),
        ], env=stack.env, capture_output=True, text=True, timeout=20 if args.mode == "abort" else 180)
        (run / f"{name}.stdout").write_text(completed.stdout)
        (run / f"{name}.stderr").write_text(completed.stderr)
        print(f"{name}: exit={completed.returncode} seconds={time.monotonic() - started:.3f}", flush=True)
        response = json.loads(completed.stdout)
        if expect_error:
            assert completed.returncode == 2 and response["status"] == "error", (name, response)
            return response
        assert completed.returncode == 0, (name, response)
        return response["data"]

    def state():
        with sqlite3.connect(f"file:{run}/stack/voom.db?mode=ro", uri=True) as database:
            database.row_factory = sqlite3.Row
            assert database.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
            return {table: [dict(row) for row in database.execute(f"SELECT * FROM {table}")]
                    for table in ("tickets", "file_versions", "file_locations", "artifact_commit_records", "artifact_commit_intents", "workflow_file_phase_summaries",
                                  "media_snapshots", "artifact_verifications", "artifact_access_plans", "audio_extract_operations", "audio_extract_operation_outputs", "audio_extract_output_lineage",
                                  "audio_synthesis_operations", "audio_synthesis_companions", "audio_synthesis_stream_lineage")}

    with CheckedBootstrap(configuration) as stack:
        root = next(root["root_id"] for root in stack.roots if root["role"] == "input")
        command(stack, "scan", ["scan", "--root", root])
        version = command(stack, "policy", ["policy", "create", "--slug", slug, "--file", run / "policy.voom"])["version"]["version_id"]
        inputs = command(stack, "input-set", ["policy", "input", "create-from-scan", "--root", root, "--slug", "publication733"])["input_set"]["input_set_id"]
        arguments = ["compliance", "execute", "--policy-version-id", version, "--input-set-id", inputs,
                     "--max-in-flight-files", "1", "--staging-root", run / "staging", "--output-dir", run / "output"]
        if args.mode == "abort":
            started = time.monotonic()
            response = command(stack, "execute", arguments, expect_error=True)
            assert time.monotonic() - started < 10, "terminal publication must settle before the convergence timeout"
            stack.proxy.assert_complete()
            failed = state()
            assert len(failed["file_versions"]) == 1
            assert not any(row["state"] == "committed" for row in failed["artifact_commit_records"])
            assert all(row["state"] == "aborted" for row in failed["artifact_commit_intents"])
            assert any(row["state"] == "failed" for row in failed["tickets"])
            assert not any(row["state"] == "leased" for row in failed["tickets"])
            failed_tickets = {row["id"] for row in failed["tickets"] if row["state"] == "failed"}
            assert all(row["status"] == "rejected" for row in failed["artifact_access_plans"] if row["ticket_id"] in failed_tickets)
            bootstrap.private_json(run / "terminal-publication-failure.json", {"response": response, "state": failed})
            assert original == (source.stat().st_mtime_ns, hashlib.sha256(source.read_bytes()).hexdigest())
            return
        executed = command(stack, "execute", arguments)
        if stack.proxy:
            # The CLI can observe atomic source completion before the owner
            # finishes retrying its deliberately lost group response.
            deadline = time.monotonic() + 10
            while args.mode == "extract" and not stack.proxy.replayed and not stack.proxy.failures and time.monotonic() < deadline:
                time.sleep(0.05)
            stack.proxy.assert_complete()
            (run / "group-proof-checks.json").write_text(json.dumps(stack.proxy.checks, indent=2))
        before = state()
        bootstrap.private_json(run / "state-before-resume.json", before)
        expected_commits = 4 if args.mode == "chain" else 2
        assert len(before["artifact_commit_records"]) == expected_commits, "every real mutation output must publish a commit"
        assert all(record["state"] == "committed" for record in before["artifact_commit_records"])
        assert all(intent["state"] == "completed" for intent in before["artifact_commit_intents"])
        if args.mode == "chain":
            outputs = [path for path in (run / "output").rglob("*") if path.is_file()]
            assert len(outputs) == 1, "the chain must produce exactly one final output"
            mutation_tickets = [ticket for ticket in before["tickets"] if json.loads(ticket["payload"]).get("operation") in ("remux", "transcode_video", "transcode_audio")]
            assert [json.loads(ticket["payload"])["rendered_payload"]["source_file_version_id"] for ticket in mutation_tickets] == [1, 2, 3, 4]
            assert before["workflow_file_phase_summaries"][-1]["produced_file_version_id"] == 5
        else:
            # Terminal promotion preserves each result location's identity and
            # updates its rooted address. Inspect the committed catalog tip.
            roots = {root["root_id"]: run / root["role"] for root in stack.roots}
            locations = {location["id"]: location for location in before["file_locations"]}
            outputs = []
            for record in before["artifact_commit_records"]:
                location = locations[record["result_file_location_id"]]
                assert location["retired_at"] is None
                outputs.append(roots[location["storage_root_id"]] / location["provider_relative_locator"])
            assert len(before["audio_extract_operations"]) == 1
            assert before["audio_extract_operations"][0]["state"] == "committed"
            assert len(before["audio_extract_operation_outputs"]) == 2
            assert {lineage["operation_output_id"] for lineage in before["audio_extract_output_lineage"]} == {
                output["id"] for output in before["audio_extract_operation_outputs"]}
        physical = []
        for output in outputs:
            facts = json.loads(subprocess.check_output(["ffprobe", "-v", "error", "-show_streams", "-of", "json", str(output)], text=True))
            physical.append(facts)
            streams = facts["streams"]
            if args.mode == "chain":
                assert any(stream.get("codec_name") == "hevc" for stream in streams)
                assert any(stream.get("codec_name") == "eac3" and stream.get("channels") == 6 for stream in streams)
                assert any(stream.get("codec_name") == "aac" and stream.get("channels") == 2 for stream in streams)
            else:
                assert len(streams) == 1 and streams[0].get("codec_name") == "opus"
        (run / "physical-output.json").write_text(json.dumps(physical, indent=2))
        output_before = [(output.stat().st_mtime_ns, hashlib.sha256(output.read_bytes()).hexdigest()) for output in outputs]
        command(stack, "resume", arguments + ["--resume-job-id", executed["summary"]["job_id"]])
        after = state()
        bootstrap.private_json(run / "state-after-resume.json", after)
        for table in before.keys() - {"workflow_file_phase_summaries"}:
            assert before[table] == after[table], f"resume repeated or changed {table}"
        # Public resume creates a new job and copies phase summaries. Those
        # summaries must retain the exact committed evidence and ticket IDs.
        evidence_fields = ("phase_ordinal", "branch_id", "ticket_ids", "produced_file_version_id",
                           "produced_file_location_id", "artifact_handle_id", "artifact_verification_id",
                           "reprobe_snapshot_id", "outcome")
        old_summaries = before["workflow_file_phase_summaries"]
        copied_summaries = after["workflow_file_phase_summaries"][len(old_summaries):]
        assert len(copied_summaries) == len(old_summaries)
        assert [[row[field] for field in evidence_fields] for row in old_summaries] == [
            [row[field] for field in evidence_fields] for row in copied_summaries]
        assert output_before == [(output.stat().st_mtime_ns, hashlib.sha256(output.read_bytes()).hexdigest()) for output in outputs]
        if args.mode == "extract":
            # A new job against the same pinned source resolves the committed
            # semantic operation. It creates a truthful fresh ticket/lease,
            # without another encoder dispatch or owner access-plan claim.
            command(stack, "reuse", arguments)
            reused = state()
            bootstrap.private_json(run / "state-after-reuse.json", reused)
            for table in before.keys() - {"tickets", "workflow_file_phase_summaries"}:
                assert before[table] == reused[table], f"semantic reuse rewrote {table}"
            assert len(reused["tickets"]) == len(before["tickets"]) + 1
            assert reused["tickets"][-1]["state"] == "succeeded"
            assert output_before == [(output.stat().st_mtime_ns, hashlib.sha256(output.read_bytes()).hexdigest()) for output in outputs]
            owner = next(process for name, process in stack.children if name == "voom-node-agent")
            stack.proxy.check_captured_operator_recovery(owner)
            stack.proxy.check_reuse_acquire(stack.proof_credential, owner)
            stack.proxy.assert_complete()
            bootstrap.private_json(run / "queue-reuse-proof.json", stack.proxy.checks)
            assert output_before == [(output.stat().st_mtime_ns, hashlib.sha256(output.read_bytes()).hexdigest()) for output in outputs]
    assert original == (source.stat().st_mtime_ns, hashlib.sha256(source.read_bytes()).hexdigest())


if __name__ == "__main__":
    main()
