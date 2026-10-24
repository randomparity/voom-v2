#!/usr/bin/env python3
"""Check persisted owner-audio reuse on a private clone, without encoding media."""

import argparse
import hashlib
import importlib.util
import json
import os
import secrets
import sqlite3
import subprocess
import time
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--workdir", type=Path, required=True)
    parser.add_argument("--mode", choices=("reuse", "late-rollback"), default="reuse")
    args = parser.parse_args()
    os.umask(0o077)
    args.workdir.mkdir(mode=0o700, parents=True, exist_ok=False)
    database = args.workdir.resolve() / "clone.db"
    with sqlite3.connect(f"file:{args.database.resolve()}?mode=ro", uri=True) as source, sqlite3.connect(database) as target:
        source.backup(target)
        context = json.loads(target.execute("SELECT report FROM artifact_verifications ORDER BY id LIMIT 1").fetchone()[0])["remote_completion"]
        token = "voom-node-v1." + secrets.token_urlsafe(32)
        digest = "voom-node-token-sha256-v1:" + hashlib.sha256(("voom-node-token-v1:" + token).encode()).hexdigest()
        now = target.execute("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')").fetchone()[0]
        # Only synthetic clone admission is made live. Historical publication,
        # source lease, original evidence and installed bytes remain unchanged.
        target.execute("UPDATE nodes SET status='active',active_incarnation_id=?,last_seen_at=?,auth_token_hash=? WHERE id=?", (context["incarnation_id"], now, digest, context["node_id"]))
        target.execute("UPDATE node_incarnations SET status='active',last_seen_at=?,ended_at=NULL,end_reason=NULL WHERE incarnation_id=?", (now, context["incarnation_id"]))
        target.execute("UPDATE workers SET status='active',last_seen_at=?,retired_at=NULL WHERE node_id=?", (now, context["node_id"]))
        if args.mode == "late-rollback":
            assert target.execute("SELECT state FROM leases WHERE id=?", (context["lease_id"],)).fetchone()[0] == "held", "late rollback needs a genuine prepublication capture"
            expiry = target.execute("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now','+60 seconds')").fetchone()[0]
            target.execute("UPDATE leases SET expires_at=? WHERE id=?", (expiry, context["lease_id"]))
            target.execute("UPDATE audio_extract_operations SET claim_expires_at=? WHERE claim_lease_id=?", (expiry, context["lease_id"]))
    credential = args.workdir / "credential"
    credential.write_text(token)
    repo = Path(__file__).resolve().parents[1]
    spec = importlib.util.spec_from_file_location("proof", repo / "scripts/issue-733-group-proof-fixture.py")
    proof = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(proof)
    binary = repo / "target/debug/voom"
    log = args.workdir / "api.stderr"
    with open(log, "x") as stderr, open(args.workdir / "api.stdout", "x") as stdout:
        process = subprocess.Popen([str(binary.with_name("voom-api")), "--database-url", "sqlite://" + str(database), "--bind", "127.0.0.1:0", "--allow-cleartext-loopback"], stdout=stdout, stderr=stderr)
        try:
            base = None
            deadline = time.monotonic() + 5
            while base is None and time.monotonic() < deadline:
                assert process.poll() is None, "clone API stopped before readiness"
                for line in log.read_text().splitlines():
                    fields = json.loads(line).get("fields", {})
                    if fields.get("event") == "listening":
                        base = "http://" + fields["bind"]
                time.sleep(0.01)
            assert base is not None
            proxy = proof.GroupProofProxy(base, database, 2, binary, os.environ.copy())
            class NoOwner:
                def send_signal(self, _signal):
                    pass
            try:
                if args.mode == "late-rollback":
                    proxy.check_late_publication_rollback(credential)
                else:
                    proxy.check_reuse_acquire(credential, NoOwner())
                assert not proxy.failures, proxy.failures
            finally:
                (args.workdir / "checks.json").write_text(json.dumps(proxy.checks, indent=2))
                (args.workdir / "failures.json").write_text(json.dumps(proxy.failures))
                proxy.server.shutdown()
                proxy.server.server_close()
                proxy.thread.join(timeout=5)
                assert not proxy.thread.is_alive()
        finally:
            process.terminate()
            try:
                code = process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
                raise AssertionError("clone API shutdown exceeded bound") from None
            credential.unlink()
            assert code == 0, "clone API shutdown failed"


if __name__ == "__main__":
    main()
