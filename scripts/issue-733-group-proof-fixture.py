"""Bounded HTTP fault fixture for the synthetic issue-733 extraction run.

Only this test proxy holds a request. Production composition has no rendezvous.
Credentials and capability-bearing bodies stay in memory and are never logged.
"""

import concurrent.futures
import contextlib
import copy
import datetime
import http.server
import hashlib
import json
import socket
import signal
import sqlite3
import subprocess
import threading
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path


class GroupProofProxy:
    def __init__(self, base_url, database, foreign_node_id, voom_bin, environment, mode="extract", apply_delay_seconds=0):
        self.apply_delay_seconds = apply_delay_seconds
        self.mode = mode
        self.scalar_checked = False
        self.shape_checked = False
        self.authorized_members = set()
        self.apply_order_checked = False
        self.base_url = base_url
        self.database = database
        self.foreign_node_id = foreign_node_id
        self.voom_bin = voom_bin
        self.environment = environment
        self.closing = False
        self.checks = []
        self.failures = []
        self.source_completion_identity = None
        self.completion_checked = False
        self.group_checked = False
        self.response_dropped = False
        self.replayed = False
        self.committed_state = None
        self.fault_lock = threading.RLock()
        self.current_case = "forward"
        proxy = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_arguments):
                pass

            def do_POST(self):
                length = int(self.headers.get("Content-Length", "0"))
                if not 0 <= length <= 2 * 1024 * 1024:
                    self.send_error(413)
                    return
                body = self.rfile.read(length)
                headers = {key: value for key, value in self.headers.items()
                           if key.lower() not in ("host", "content-length", "connection")}
                try:
                    encoded = json.loads(body)
                    if proxy.mode == "abort" and self.path.endswith("/authorize") and not proxy.scalar_checked:
                        proxy.scalar_checked = True
                        with sqlite3.connect(proxy.database) as database:
                            source = database.execute("SELECT json_extract(report, '$.remote_completion.source_location_id') FROM artifact_verifications ORDER BY id LIMIT 1").fetchone()[0]
                        with proxy.changed_row("file_locations", "epoch", source, lambda value: value + 1):
                            response = proxy.forward(self.path, body, headers)
                        assert response[0] == 409
                        proxy.checks.append({"case": "authorization-source-drift-aborts-publication", "status": response[0]})
                    elif proxy.mode == "chain" and "/artifact/commit/" in self.path and self.path.endswith("/complete") and not proxy.scalar_checked:
                        proxy.scalar_checked = True
                        proxy.check_scalar(self.path, encoded, headers)
                        response = proxy.forward(self.path, body, headers)
                    elif proxy.mode == "chain" and "/execution/lease/" in self.path and self.path.endswith("/complete") and encoded.get("result", {}).get("agent_observed", {}).get("outputs") and not proxy.shape_checked:
                        proxy.shape_checked = True
                        proxy.check_source_shape(self.path, encoded, headers)
                        response = proxy.forward(self.path, body, headers)
                    elif proxy.mode == "extract" and "/execution/lease/" in self.path and self.path.endswith("/complete"):
                        result = encoded.get("result", {})
                        if result.get("agent_observed", {}).get("outputs"):
                            key = next(value for name, value in headers.items() if name.lower() == "x-voom-idempotency-key")
                            identity = hashlib.sha256(key.encode() + b"\0" + body).digest()
                            if proxy.source_completion_identity is None:
                                proxy.source_completion_identity = identity
                            else:
                                assert identity == proxy.source_completion_identity, "completion window retry changed frozen request identity"
                        if result.get("agent_observed", {}).get("outputs") and not proxy.completion_checked:
                            proxy.completion_checked = True
                            proxy.check_owner_receipt(self.path, encoded, headers)
                            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
                                futures = [executor.submit(proxy.forward, self.path, body, headers) for _ in range(2)]
                                responses = [future.result() for future in futures]
                            assert responses[0] == responses[1], "duplicate source completion changed its replay"
                            proxy.checks.append({"case": "duplicate-source-completion", "status": responses[0][0]})
                            response = responses[0]
                        else:
                            response = proxy.forward(self.path, body, headers)
                    elif proxy.mode == "extract" and self.path.endswith("/authorize"):
                        response = proxy.forward(self.path, body, headers)
                        if response[0] == 200:
                            proxy.authorized_members.add(int(self.path.split("/")[-2]))
                    elif proxy.mode == "extract" and self.path.endswith("/applying"):
                        with sqlite3.connect(proxy.database) as database:
                            members = {row[0] for row in database.execute("SELECT id FROM artifact_commit_intents")}
                        assert proxy.authorized_members == members and len(members) == 2
                        if not proxy.apply_order_checked:
                            proxy.apply_order_checked = True
                            if proxy.apply_delay_seconds:
                                time.sleep(proxy.apply_delay_seconds)
                                proxy.checks.append({"case": "healthy-owner-preapply-delay", "seconds": proxy.apply_delay_seconds})
                            proxy.checks.append({"case": "all-member-authorizations-before-first-apply", "status": 200})
                        response = proxy.forward(self.path, body, headers)
                    elif "/artifact/commit/group/" in self.path and self.path.endswith("/complete"):
                        if not proxy.group_checked:
                            proxy.group_checked = True
                            proxy.check_group(self.path, encoded, headers)
                            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
                                futures = [executor.submit(proxy.forward, self.path, body, headers) for _ in range(2)]
                                responses = [future.result() for future in futures]
                            assert responses[0] == responses[1] and responses[0][0] == 200, "concurrent group completion did not converge"
                            proxy.committed_state = proxy.publication_state()
                            proxy.checks.append({"case": "concurrent-group-completion", "status": 200})
                            # Lose the actual successful response, forcing the
                            # owner's frozen request to replay consumed fences.
                            proxy.response_dropped = True
                            self.close_connection = True
                            self.connection.shutdown(socket.SHUT_RDWR)
                            self.connection.close()
                            return
                        response = proxy.forward(self.path, body, headers)
                        assert response[0] == 200, "lost group response could not replay"
                        assert proxy.publication_state() == proxy.committed_state, "group replay changed publication identity"
                        proxy.replayed = True
                        proxy.checks.append({"case": "lost-group-response-replay", "status": 200})
                    else:
                        if self.path.endswith("/heartbeat"):
                            with proxy.fault_lock:
                                response = proxy.forward(self.path, body, headers)
                        else:
                            response = proxy.forward(self.path, body, headers)
                except Exception as error:
                    # Retain only the assertion/error class, never an exception
                    # containing a capability-bearing request or HTTP headers.
                    proxy.failures.append(proxy.current_case + ":" + type(error).__name__)
                    response = (500, b'{"error":"synthetic group fixture failed"}')
                if proxy.apply_delay_seconds and "/execution/lease/" in self.path and self.path.endswith("/complete") and response[0] == 408:
                    assert json.loads(response[1])["error"]["code"] == "REQUEST_TIMEOUT"
                    proxy.checks.append({"case": "healthy-completion-response-window", "status": 408})
                self.send_response(response[0])
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(response[1])))
                self.end_headers()
                try:
                    self.wfile.write(response[1])
                except (BrokenPipeError, ConnectionResetError):
                    if not proxy.closing:
                        state = proxy.publication_state()
                        expected_terminal = False
                        if proxy.mode == "abort":
                            with sqlite3.connect(proxy.database) as database:
                                failed = database.execute("SELECT COUNT(*) FROM tickets WHERE state = 'failed'").fetchone()[0]
                                leased = database.execute("SELECT COUNT(*) FROM tickets WHERE state = 'leased'").fetchone()[0]
                            expected_terminal = failed > 0 and leased == 0 and bool(state["intents"]) and all(row[1] == "aborted" for row in state["intents"]) and len(state["versions"]) == 1
                        if not expected_terminal and (not state["commits"] or any(row[1] != "committed" for row in state["commits"])):
                            proxy.failures.append("client-cancelled-before-publication")

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def forward(self, path, body, headers, base_url=None):
        request = urllib.request.Request((base_url or self.base_url) + path, data=body, headers=headers, method="POST")
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                return response.status, response.read()
        except urllib.error.HTTPError as error:
            return error.code, error.read()

    def publication_state(self, path=None):
        with sqlite3.connect(f"file:{path or self.database}?mode=ro", uri=True) as database:
            return {name: database.execute(query).fetchall() for name, query in {
                "versions": "SELECT id, file_asset_id, produced_from_version_id FROM file_versions ORDER BY id",
                "commits": "SELECT id, state, result_file_version_id, result_file_location_id FROM artifact_commit_records ORDER BY id",
                "intents": "SELECT id, state FROM artifact_commit_intents ORDER BY id",
                "operations": "SELECT id, state, dispatch_generation FROM audio_extract_operations ORDER BY id",
                "lineage": "SELECT id, operation_output_id, result_file_version_id FROM audio_extract_output_lineage ORDER BY id",
                "bundles": "SELECT id, bundle_id, file_asset_id, role FROM asset_bundle_members ORDER BY id",
                "handles": "SELECT id FROM artifact_handles ORDER BY id",
                "verifications": "SELECT id FROM artifact_verifications ORDER BY id",
                "snapshots": "SELECT id FROM media_snapshots ORDER BY id",
                "tickets": "SELECT id, state FROM tickets ORDER BY id",
                "leases": "SELECT id, state FROM leases ORDER BY id",
                "access_plans": "SELECT id, status FROM artifact_access_plans ORDER BY id",
            }.items()}

    def reject(self, label, path, encoded, headers, expected, before, code=None):
        self.current_case = label
        headers = dict(headers)
        headers["X-Voom-Idempotency-Key"] = "fixture-" + uuid.uuid4().hex
        status, body = self.forward(path, json.dumps(encoded).encode(), headers)
        self.checks.append({"case": label, "status": status, "expected": expected})
        assert status == expected, f"{label}: unexpected status"
        if code:
            assert json.loads(body)["error"]["code"] == code, f"{label}: unexpected error category"
        assert self.publication_state() == before, f"{label}: published partial state"

    @contextlib.contextmanager
    def changed_row(self, table, column, row_id, change):
        assert (table, column) in {
            ("library_roots", "root_epoch"), ("library_roots", "owner_node_id"),
            ("file_locations", "epoch"), ("audio_extract_operations", "claim_expires_at"),
            ("audio_extract_operations", "worker_result"),
            ("audio_extract_dispatch_attempts", "idempotency_key"),
            ("artifact_verifications", "report"),
            ("artifact_verifications", "expected_size_bytes"),
            ("artifact_verifications", "worker_id"),
            ("tickets", "payload"),
            ("leases", "expires_at"),
        }
        # Hold only fixture heartbeat forwarding while temporarily injecting
        # metadata drift. No production transaction or request is held here.
        with self.fault_lock:
            with sqlite3.connect(self.database) as database:
                old = database.execute(f"SELECT {column} FROM {table} WHERE id = ?", (row_id,)).fetchone()[0]
                database.execute(f"UPDATE {table} SET {column} = ? WHERE id = ?", (change(old), row_id))
            try:
                yield
            finally:
                with sqlite3.connect(self.database) as database:
                    database.execute(f"UPDATE {table} SET {column} = ? WHERE id = ?", (old, row_id))

    @contextlib.contextmanager
    def bound_verification(self, verification, ticket, lease):
        with self.fault_lock:
            with sqlite3.connect(self.database) as database:
                old = database.execute("SELECT workflow_ticket_id, workflow_lease_id FROM artifact_verifications WHERE id=?", (verification,)).fetchone()
                assert old == (None, None)
                database.execute("UPDATE artifact_verifications SET workflow_ticket_id=?, workflow_lease_id=? WHERE id=?", (ticket, lease, verification))
            try:
                yield
            finally:
                with sqlite3.connect(self.database) as database:
                    database.execute("UPDATE artifact_verifications SET workflow_ticket_id=?, workflow_lease_id=? WHERE id=?", (*old, verification))

    def check_owner_receipt(self, path, encoded, headers):
        before = self.publication_state()
        assert not before["commits"] and len(before["versions"]) == 1
        with sqlite3.connect(self.database) as database:
            attempt, stored_key = database.execute("SELECT id,idempotency_key FROM audio_extract_dispatch_attempts ORDER BY id LIMIT 1").fetchone()
        lease = int(path.split("/")[-2])
        assert stored_key == f"{encoded['incarnation_id']}-{lease}"
        with self.changed_row("audio_extract_dispatch_attempts", "idempotency_key", attempt, lambda _value: "fixture-other-dispatch"):
            self.reject("receipt-rejects-wrong-dispatch-key", path, encoded, headers, 503, before, "DB_UNREACHABLE")
        missing = copy.deepcopy(encoded)
        for output in missing["result"]["agent_observed"]["outputs"]:
            output.pop("probe")
        self.reject("legacy-missing-probe", path, missing, headers, 500, before, "MALFORMED_WORKER_RESULT")
        changed = copy.deepcopy(encoded)
        changed["result"]["agent_observed"]["outputs"][0]["probe"]["result"]["pre_probe"]["content_hash"] = "blake3:" + "0" * 64
        self.reject("probe-facts-mismatch", path, changed, headers, 500, before, "MALFORMED_WORKER_RESULT")
        for field, value in (("output_id", "foreign-output"), ("selection", {"snapshot_stream_id": "foreign-stream", "provider_stream_index": 99})):
            changed = copy.deepcopy(encoded)
            changed["result"]["outputs"][1][field] = value
            self.reject("extract-result-" + field, path, changed, headers, 500, before, "MALFORMED_WORKER_RESULT")
        changed = copy.deepcopy(encoded)
        changed["result"]["outputs"].append(copy.deepcopy(changed["result"]["outputs"][0]))
        self.reject("extract-result-extra-member", path, changed, headers, 500, before, "MALFORMED_WORKER_RESULT")
        changed = copy.deepcopy(encoded)
        changed["result"]["outputs"].reverse()
        self.reject("extract-result-reordered", path, changed, headers, 500, before, "MALFORMED_WORKER_RESULT")
        changed = copy.deepcopy(encoded)
        changed["result"]["selected_snapshot_stream_id"] = "foreign-stream"
        self.reject("extract-result-first-projection", path, changed, headers, 500, before, "MALFORMED_WORKER_RESULT")

    def check_scalar(self, path, encoded, headers):
        before = self.publication_state()
        intent_id = int(path.split("/")[-2])
        with sqlite3.connect(self.database) as database:
            verification, report = database.execute("SELECT verification_id, report FROM artifact_commit_intents JOIN artifact_verifications ON artifact_verifications.id = verification_id WHERE artifact_commit_intents.id = ?", (intent_id,)).fetchone()
        report = json.loads(report)
        def changed_probe(value):
            changed = json.loads(value)
            assert changed["owner_probe"]["worker_id"] != changed["remote_completion"]["worker_id"]
            changed["owner_probe"]["worker_id"] = changed["remote_completion"]["worker_id"]
            return json.dumps(changed)
        with self.changed_row("artifact_verifications", "report", verification, changed_probe):
            self.reject("scalar-stored-probe-attribution", path, encoded, headers, 503, before, "DB_UNREACHABLE")
        source = report["remote_completion"]["source_location_id"]
        with self.changed_row("file_locations", "epoch", source, lambda value: value + 1):
            self.reject("scalar-original-source-epoch", path, encoded, headers, 409, before)

    def check_source_shape(self, path, encoded, headers):
        before = self.publication_state()
        lease = int(path.split("/")[-2])
        with sqlite3.connect(self.database) as database:
            ticket = database.execute("SELECT ticket_id FROM leases WHERE id = ?", (lease,)).fetchone()[0]
        def missing_dispatch(value):
            payload = json.loads(value)
            payload["rendered_payload"].pop("media_dispatch")
            return json.dumps(payload)
        with self.changed_row("tickets", "payload", ticket, missing_dispatch):
            self.reject("required-mutation-missing-dispatch", path, encoded, headers, 503, before, "DB_UNREACHABLE")
        def wrong_dispatch(value):
            payload = json.loads(value)
            original = payload["rendered_payload"]["media_dispatch"]
            payload["rendered_payload"]["media_dispatch"] = {"operation": "probe", "schema": original["schema"], "source": original["source"], "expected": original["expected"]}
            return json.dumps(payload)
        with self.changed_row("tickets", "payload", ticket, wrong_dispatch):
            self.reject("required-mutation-wrong-dispatch", path, encoded, headers, 503, before, "DB_UNREACHABLE")

    def check_group(self, path, encoded, headers):
        before = self.publication_state()
        assert len(before["versions"]) == 1 and not before["lineage"]
        assert len(before["commits"]) == 2 and all(row[1] == "pending" for row in before["commits"])
        assert before["operations"][0][1] == "prepared"
        proof = encoded["proof"]
        with sqlite3.connect(self.database) as database:
            acquired, ttl = database.execute("SELECT acquired_at, ttl_seconds FROM leases WHERE id=?", (proof["group"]["claim_lease_id"],)).fetchone()
        assert ttl == 5
        original_deadline = datetime.datetime.fromisoformat(acquired.replace("Z", "+00:00")).timestamp() + ttl
        time.sleep(max(0, original_deadline + 0.3 - time.time()))
        with sqlite3.connect(self.database) as database:
            lease_expiry, claim_expiry = database.execute("SELECT leases.expires_at, audio_extract_operations.claim_expires_at FROM leases JOIN audio_extract_operations ON claim_lease_id=leases.id WHERE leases.id=?", (proof["group"]["claim_lease_id"],)).fetchone()
        assert all(datetime.datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp() > time.time() for value in (lease_expiry, claim_expiry))
        self.checks.append({"case": "held-completion-heartbeat-crosses-original-ttl", "status": 200})
        capture = self.database.parent / "prepublication-recovery.db"
        with sqlite3.connect(f"file:{self.database}?mode=ro", uri=True) as source, sqlite3.connect(capture) as target:
            source.backup(target)
        self.recovery_capture = (capture, copy.deepcopy(encoded), dict(headers), path)

    def check_group_negatives(self, path, encoded, headers):
        before = self.publication_state()
        proof = encoded["proof"]
        with sqlite3.connect(self.database) as database:
            attempt_id, stored_key = database.execute("SELECT id,idempotency_key FROM audio_extract_dispatch_attempts ORDER BY id LIMIT 1").fetchone()
            context = json.loads(database.execute("SELECT report FROM artifact_verifications ORDER BY id LIMIT 1").fetchone()[0])["remote_completion"]
        assert stored_key == f"{context['incarnation_id']}-{context['lease_id']}"
        with self.changed_row("audio_extract_dispatch_attempts", "idempotency_key", attempt_id, lambda _value: "fixture-other-dispatch"):
            self.reject("first-publication-rejects-wrong-dispatch-key", path, encoded, headers, 503, before, "DB_UNREACHABLE")
        with sqlite3.connect(self.database) as database:
            verification, report, mutation_worker = database.execute("SELECT v.id, v.report, v.worker_id FROM artifact_verifications v JOIN artifact_commit_intents i ON i.verification_id=v.id WHERE i.id=?", (proof["members"][-1]["intent_id"],)).fetchone()
        report = json.loads(report)
        probe_worker = report["owner_probe"]["worker_id"]
        assert mutation_worker == report["remote_completion"]["worker_id"] and mutation_worker != probe_worker
        with self.changed_row("artifact_verifications", "worker_id", verification, lambda _value: probe_worker):
            self.reject("first-publication-rejects-probe-as-mutation-worker", path, encoded, headers, 503, before, "DB_UNREACHABLE")
        with self.bound_verification(verification, report["remote_completion"]["ticket_id"], proof["group"]["claim_lease_id"]):
            self.reject("first-audio-publication-rejects-workflow-binding", path, encoded, headers, 503, before, "DB_UNREACHABLE")
        def wrong_worker_result(value):
            changed = json.loads(value)
            for key in ("input_pre", "input_post"):
                changed[key]["content_hash"] = "fixture-other-input"
            return json.dumps(changed)
        with self.changed_row("audio_extract_operations", "worker_result", proof["group"]["operation_id"], wrong_worker_result):
            self.reject("first-publication-rejects-contradictory-worker-result", path, encoded, headers, 503, before, "DB_UNREACHABLE")
        with sqlite3.connect(self.database) as database:
            source_location = database.execute("SELECT json_extract(report, '$.remote_completion.source_location_id') FROM artifact_verifications ORDER BY id LIMIT 1").fetchone()[0]
        with self.changed_row("file_locations", "epoch", source_location, lambda value: value + 1):
            self.reject("original-source-location-epoch", path, encoded, headers, 409, before)
        for member in proof["members"]:
            self.reject("scalar-group-member", f'/v1/artifact/commit/{member["intent_id"]}/complete',
                        {"node_id": encoded["node_id"], "incarnation_id": encoded["incarnation_id"], "fence_hex": member["fence_hex"]}, headers, 409, before)
        cases = []
        missing = copy.deepcopy(encoded)
        missing["proof"]["members"].pop()
        cases.append(("missing-member", missing))
        extra = copy.deepcopy(encoded)
        extra["proof"]["members"].append(copy.deepcopy(extra["proof"]["members"][0]))
        cases.append(("extra-member", extra))
        duplicate = copy.deepcopy(encoded)
        duplicate["proof"]["members"][1] = copy.deepcopy(duplicate["proof"]["members"][0])
        cases.append(("duplicate-member", duplicate))
        reversed_members = copy.deepcopy(encoded)
        reversed_members["proof"]["members"].reverse()
        cases.append(("reordered-members", reversed_members))
        generation = copy.deepcopy(encoded)
        generation["proof"]["group"]["generation"] += 1
        cases.append(("stale-generation", generation))
        claim = copy.deepcopy(encoded)
        claim["proof"]["group"]["claim_token"] = "wrong-claim"
        cases.append(("wrong-claim", claim))
        fence = copy.deepcopy(encoded)
        fence["proof"]["members"][1]["fence_hex"] = "0" * 64
        cases.append(("wrong-member-fence", fence))
        receipt = copy.deepcopy(encoded)
        receipt["proof"]["members"][1]["applied"]["content_hash"] = "blake3:" + "0" * 64
        cases.append(("wrong-applied-receipt", receipt))
        for label, changed in cases:
            self.reject(label, path, changed, headers, 409, before)
        unknown = copy.deepcopy(encoded)
        unknown["proof"]["group"]["unknown"] = True
        self.reject("unknown-proof-field", path, unknown, headers, 400, before)
        untrusted = dict(headers)
        untrusted.pop("Authorization", None)
        untrusted.pop("authorization", None)
        self.reject("unauthenticated-group", path, encoded, untrusted, 401, before)
        incarnation = copy.deepcopy(encoded)
        incarnation["incarnation_id"] = "0" * 32
        self.reject("stale-incarnation", path, incarnation, headers, 409, before)
        with sqlite3.connect(self.database) as database:
            target_root, staging_location, verification = database.execute(
                "SELECT target_storage_root_id, staging_location_id, verification_id FROM artifact_commit_intents WHERE id = ?",
                (proof["members"][-1]["intent_id"],),
            ).fetchone()
        with self.changed_row("library_roots", "root_epoch", target_root, lambda value: value + 1):
            self.reject("target-root-epoch-drift", path, encoded, headers, 409, before)
        with self.changed_row("library_roots", "owner_node_id", target_root, lambda _value: self.foreign_node_id):
            self.reject("target-owner-drift", path, encoded, headers, 409, before)
        with self.changed_row("file_locations", "epoch", staging_location, lambda value: value + 1):
            self.reject("last-member-location-epoch-drift", path, encoded, headers, 409, before)
        with self.changed_row("audio_extract_operations", "claim_expires_at", proof["group"]["operation_id"], lambda _value: "1970-01-01T00:00:00Z"):
            self.reject("expired-operation-claim", path, encoded, headers, 409, before)
        def corrupt_report(value):
            report = json.loads(value)
            report["owner_probe"]["unknown"] = True
            return json.dumps(report)
        with self.changed_row("artifact_verifications", "report", verification, corrupt_report):
            self.reject("corrupt-last-member-before-claim-conflict", path, claim, headers, 503, before, "DB_UNREACHABLE")

    def check_captured_operator_recovery(self, node_process):
        with sqlite3.connect(self.database) as database:
            self.recovery_physical_paths = [Path(root) / relative for root, relative in database.execute(
                "SELECT root.provider_locator, location.provider_relative_locator FROM artifact_commit_records AS commit_record "
                "JOIN file_locations AS location ON location.id=commit_record.result_file_location_id "
                "JOIN library_roots AS root ON root.id=location.storage_root_id ORDER BY commit_record.id")]
        original = self.database
        capture, encoded, headers, path = self.recovery_capture
        node_process.send_signal(signal.SIGSTOP)
        try:
            self.database = capture
            # No owner/waiter runs in this captured database. Give the isolated
            # positive recovery control a bounded live claim; expiry negatives
            # below independently expire this exact original source lease.
            with sqlite3.connect(capture) as database:
                expiry = (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=60)).isoformat().replace("+00:00", "Z")
                database.execute("UPDATE leases SET expires_at=? WHERE id=?", (expiry, encoded["proof"]["group"]["claim_lease_id"]))
                database.execute("UPDATE audio_extract_operations SET claim_expires_at=? WHERE id=?", (expiry, encoded["proof"]["group"]["operation_id"]))
            with self.isolated_api(capture) as base:
                old_base = self.base_url
                self.base_url = base
                try:
                    self.check_group_negatives(path, encoded, headers)
                    self.check_operator_recovery(encoded, headers, path)
                finally:
                    self.base_url = old_base
        finally:
            self.database = original
            node_process.send_signal(signal.SIGCONT)

    @contextlib.contextmanager
    def isolated_api(self, clone):
        log = clone.with_suffix(".checks-api.stderr")
        with open(log, "x") as stderr, open(clone.with_suffix(".checks-api.stdout"), "x") as stdout:
            process = subprocess.Popen([str(self.voom_bin.with_name("voom-api")), "--database-url", "sqlite://" + str(clone), "--bind", "127.0.0.1:0", "--allow-cleartext-loopback"], env=self.environment, stdout=stdout, stderr=stderr)
            try:
                deadline = time.monotonic() + 5
                base = None
                while base is None and time.monotonic() < deadline:
                    assert process.poll() is None, "isolated API stopped before readiness"
                    for line in log.read_text().splitlines():
                        fields = json.loads(line).get("fields", {})
                        if fields.get("event") == "listening":
                            base = "http://" + fields["bind"]
                    time.sleep(0.01)
                assert base is not None
                yield base
            finally:
                process.terminate()
                try:
                    code = process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
                    raise AssertionError("isolated checks API shutdown exceeded bound") from None
                assert code == 0, "isolated checks API shutdown failed"

    def check_operator_recovery(self, encoded, headers, group_path):
        # Copy the actual durable crash window after all applies, before the
        # normal group request. Run checks after live completion. No agent or completion waiter runs in a clone;
        # the operator command must converge solely from persisted evidence.
        directory = self.database.parent / "group-recovery"
        directory.mkdir(mode=0o700)
        with sqlite3.connect(f"file:{self.database}?mode=ro", uri=True) as source:
            handle = source.execute("SELECT artifact_handle_id FROM artifact_commit_intents WHERE id = ?",
                                    (encoded["proof"]["members"][0]["intent_id"],)).fetchone()[0]
            paths = [Path(root) / relative for root, relative in source.execute(
                "SELECT root.provider_locator, intent.target_provider_relative_locator FROM artifact_commit_intents AS intent "
                "JOIN library_roots AS root ON root.id = intent.target_storage_root_id ORDER BY intent.id")]
        paths = getattr(self, "recovery_physical_paths", paths)
        physical = [(path.stat().st_mtime_ns, hashlib.sha256(path.read_bytes()).hexdigest()) for path in paths]
        for name, mode in (("all-applied", "applied"), ("one-unresolved", "unresolved"), ("expired-source", "expired")):
            unresolved = mode == "unresolved"
            clone = directory / (name + ".db")
            with sqlite3.connect(f"file:{self.database}?mode=ro", uri=True) as source, sqlite3.connect(clone) as target:
                source.backup(target)
                if unresolved:
                    intent_id = encoded["proof"]["members"][-1]["intent_id"]
                    receipt = json.loads(target.execute("SELECT receipt FROM artifact_commit_intents WHERE id = ?", (intent_id,)).fetchone()[0])
                    target.execute("UPDATE artifact_commit_intents SET receipt = ?, intent_epoch = intent_epoch + 1 WHERE id = ?",
                                   (json.dumps({"kind": "applying", "reported_at": receipt["reported_at"]}), intent_id))
                if mode == "expired":
                    target.execute("UPDATE leases SET expires_at='1970-01-01T00:00:00Z' WHERE id=?", (encoded["proof"]["group"]["claim_lease_id"],))
            before = self.publication_state(clone)
            command = [str(self.voom_bin), "--database-url", "sqlite://" + str(clone),
                       "artifact", "recover-commit", "--artifact-handle-id", str(handle)]
            completed = subprocess.run(command, env=self.environment, capture_output=True, text=True, timeout=15)
            (directory / (name + ".stdout")).write_text(completed.stdout)
            (directory / (name + ".stderr")).write_text(completed.stderr)
            response = json.loads(completed.stdout)
            after = self.publication_state(clone)
            if mode == "expired":
                assert completed.returncode == 2 and response["error"]["code"] == "CONFLICT"
                assert before == after, "expired-source recovery published output"
                self.check_expired_group_api(clone, group_path, encoded, headers)
            elif unresolved:
                assert completed.returncode == 2 and response["error"]["code"] == "CONFLICT"
                for table in before.keys() - {"commits", "intents", "operations"}:
                    assert before[table] == after[table], "partial recovery published a sibling"
                assert all(row[1] == "recovery_required" for row in after["commits"])
                assert all(row[1] == "recovery_required" for row in after["intents"])
                assert after["operations"][0][1] == "recovery_required"
            else:
                assert completed.returncode == 0 and response["status"] == "ok"
                assert all(row[1] == "committed" for row in after["commits"])
                assert all(row[1] == "completed" for row in after["intents"])
                assert after["operations"][0][1] == "committed" and len(after["lineage"]) == 2
                with sqlite3.connect(clone) as database:
                    assert database.execute("SELECT state FROM leases WHERE id = ?", (encoded["proof"]["group"]["claim_lease_id"],)).fetchone()[0] == "released"
                    assert database.execute("SELECT state FROM tickets WHERE id = (SELECT ticket_id FROM leases WHERE id = ?)",
                                            (encoded["proof"]["group"]["claim_lease_id"],)).fetchone()[0] == "succeeded"
                replay = subprocess.run(command, env=self.environment, capture_output=True, text=True, timeout=15)
                assert replay.returncode == 2 and json.loads(replay.stdout)["error"]["code"] == "CONFLICT"
                assert self.publication_state(clone) == after
                self.checks.append({"case": "operator-recovery-terminal-replay", "exit_code": replay.returncode})
            self.checks.append({"case": "operator-recovery-" + name, "exit_code": completed.returncode})
        assert physical == [(path.stat().st_mtime_ns, hashlib.sha256(path.read_bytes()).hexdigest()) for path in paths]

    def check_expired_group_api(self, clone, path, encoded, headers):
        # Exercise ordinary grouped completion against the actual expired-source
        # capture with no background waiter or owner, independently of recovery.
        before = self.publication_state(clone)
        log = clone.with_suffix(".api.stderr")
        with open(log, "x", encoding="utf-8") as stderr, open(clone.with_suffix(".api.stdout"), "x", encoding="utf-8") as stdout:
            process = subprocess.Popen([str(self.voom_bin.with_name("voom-api")), "--database-url", "sqlite://" + str(clone), "--bind", "127.0.0.1:0", "--allow-cleartext-loopback"], env=self.environment, stdout=stdout, stderr=stderr)
            try:
                deadline = time.monotonic() + 5
                base = None
                while base is None and time.monotonic() < deadline:
                    assert process.poll() is None, "isolated API stopped before readiness"
                    for line in log.read_text().splitlines():
                        fields = json.loads(line).get("fields", {})
                        if fields.get("event") == "listening":
                            base = "http://" + fields["bind"]
                    if base is None:
                        time.sleep(0.01)
                assert base is not None
                request_headers = dict(headers)
                request_headers["X-Voom-Idempotency-Key"] = "fixture-expired-group-" + uuid.uuid4().hex
                status, body = self.forward(path, json.dumps(encoded).encode(), request_headers, base)
                assert status == 409 and json.loads(body)["error"]["code"] == "CONFLICT"
                assert self.publication_state(clone) == before
                self.checks.append({"case": "ordinary-group-complete-rejects-expired-source", "status": status})
            finally:
                if process.poll() is None:
                    process.terminate()
                try:
                    code = process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
                    raise AssertionError("isolated API did not stop boundedly") from None
                assert code == 0, "isolated API shutdown failed"
                self.checks.append({"case": "isolated-expiry-api-shutdown", "exit_code": code})

    def check_reuse_acquire(self, credential, node_process):
        # Pause only this fixture's idle owner while submitting authenticated
        # acquire/replay requests. Seed fresh queue rows in the synthetic DB;
        # no encoder receives the unrelated ready ticket.
        node_process.send_signal(signal.SIGSTOP)
        try:
            with sqlite3.connect(self.database) as database:
                verification, report = database.execute("SELECT id, report FROM artifact_verifications ORDER BY id LIMIT 1").fetchone()
                context = json.loads(report)["remote_completion"]
                kind, payload = database.execute("SELECT kind, payload FROM tickets WHERE id = ?", (context["ticket_id"],)).fetchone()
                now = database.execute("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')").fetchone()[0]
                job = database.execute("INSERT INTO jobs (kind,state,priority,created_at,updated_at,epoch) VALUES ('workflow','open',0,?,?,0)", (now, now)).lastrowid
                def ticket_row(operation, priority):
                    return database.execute("INSERT INTO tickets (job_id,kind,state,priority,payload,result,attempt,max_attempts,next_eligible_at,created_at,state_changed_at,epoch) VALUES (?,?,'ready',?,?,NULL,0,1,?,?,?,0)", (job, operation, priority, payload, now, now, now)).lastrowid
                reused = ticket_row(kind, 100)
                remaining = ticket_row("extract_audio", 50)
                reports_before = database.execute("SELECT id, report FROM artifact_verifications ORDER BY id").fetchall()
                attempt, ordinal, original_path = database.execute("SELECT attempt_id, ordinal, path FROM audio_extract_dispatch_attempt_paths ORDER BY attempt_id,ordinal DESC LIMIT 1").fetchone()
            headers = {"Content-Type": "application/json", "Authorization": "Bearer " + Path(credential).read_text().strip(), "X-Voom-Idempotency-Key": "fixture-reuse-" + uuid.uuid4().hex}
            body = {"node_id": context["node_id"], "worker_id": context["worker_id"], "incarnation_id": context["incarnation_id"], "lease_ttl_seconds": 30}
            before = self.publication_state()
            with sqlite3.connect(self.database) as database:
                stored_key = database.execute("SELECT idempotency_key FROM audio_extract_dispatch_attempts WHERE id=?", (attempt,)).fetchone()[0]
            assert stored_key == f"{context['incarnation_id']}-{context['lease_id']}"
            with self.changed_row("audio_extract_dispatch_attempts", "idempotency_key", attempt, lambda _value: "fixture-other-dispatch"):
                self.reject("reuse-rejects-wrong-original-dispatch-key", "/v1/execution/lease/acquire", body, headers, 503, before, "DB_UNREACHABLE")
            with self.bound_verification(verification, context["ticket_id"], context["lease_id"]):
                self.reject("reuse-rejects-original-bound-verification", "/v1/execution/lease/acquire", body, headers, 503, before, "DB_UNREACHABLE")
            probe_worker = json.loads(report)["owner_probe"]["worker_id"]
            with self.changed_row("artifact_verifications", "worker_id", verification, lambda _value: probe_worker):
                self.reject("reuse-rejects-probe-as-mutation-worker", "/v1/execution/lease/acquire", body, headers, 503, before, "DB_UNREACHABLE")
            for case in ("missing", "null", "unknown", "invalid-probe", "different-probe"):
                def corrupt_observation(value, case=case):
                    changed = json.loads(value)
                    result = changed["remote_completion"]["result"]
                    if case == "missing":
                        result.pop("agent_observed")
                    elif case == "null":
                        result["agent_observed"] = None
                    elif case == "unknown":
                        result["agent_observed"]["outputs"][0]["unexpected"] = True
                    else:
                        result["agent_observed"]["outputs"][0]["probe"]["worker_id"] = 0 if case == "invalid-probe" else context["worker_id"]
                    return json.dumps(changed)
                with self.changed_row("artifact_verifications", "report", verification, corrupt_observation):
                    self.reject("reuse-rejects-observation-" + case, "/v1/execution/lease/acquire", body, headers, 503, before, "DB_UNREACHABLE")
            def wrong_worker_result(value):
                changed = json.loads(value)
                for key in ("input_pre", "input_post"):
                    changed[key]["content_hash"] = "fixture-other-input"
                return json.dumps(changed)
            with self.changed_row("audio_extract_operations", "worker_result", self.publication_state()["operations"][0][0], wrong_worker_result):
                self.reject("reuse-rejects-contradictory-worker-result", "/v1/execution/lease/acquire", body, headers, 503, before, "DB_UNREACHABLE")
            with sqlite3.connect(self.database) as database:
                database.execute("UPDATE audio_extract_dispatch_attempt_paths SET path=? WHERE attempt_id=? AND ordinal=?", (original_path + ".foreign", attempt, ordinal))
            try:
                self.reject("reuse-rejects-wrong-attempt-path", "/v1/execution/lease/acquire", body, headers, 503, before, "DB_UNREACHABLE")
            finally:
                with sqlite3.connect(self.database) as database:
                    database.execute("UPDATE audio_extract_dispatch_attempt_paths SET path=? WHERE attempt_id=? AND ordinal=?", (original_path, attempt, ordinal))
            def wrong_source(value):
                changed = json.loads(value)
                changed["remote_completion"]["source_file_version_id"] += 1
                return json.dumps(changed)
            with self.changed_row("artifact_verifications", "report", verification, wrong_source):
                self.reject("reuse-rejects-wrong-original-source", "/v1/execution/lease/acquire", body, headers, 503, before, "DB_UNREACHABLE")
            with self.changed_row("artifact_verifications", "expected_size_bytes", verification, lambda value: value + 1):
                self.reject("reuse-rejects-wrong-verification-facts", "/v1/execution/lease/acquire", body, headers, 503, before, "DB_UNREACHABLE")
            encoded = json.dumps(body).encode()
            response = self.forward("/v1/execution/lease/acquire", encoded, headers)
            assert response[0] == 200
            outcome = json.loads(response[1])["data"]
            assert outcome["outcome"] == "leased" and outcome["ticket_id"] == remaining
            after = self.publication_state()
            for name in before.keys() - {"tickets", "leases", "access_plans"}:
                assert before[name] == after[name], "semantic reuse changed canonical publication"
            assert len(after["leases"]) == len(before["leases"]) + 2
            assert len(after["access_plans"]) == len(before["access_plans"]) + 1
            assert self.forward("/v1/execution/lease/acquire", encoded, headers) == response
            assert self.publication_state() == after, "acquire replay repeated admission"
            with sqlite3.connect(self.database) as database:
                assert database.execute("SELECT state FROM tickets WHERE id=?", (reused,)).fetchone()[0] == "succeeded"
                assert database.execute("SELECT state FROM leases WHERE ticket_id=?", (reused,)).fetchone()[0] == "released"
                assert database.execute("SELECT COUNT(*) FROM artifact_access_plans WHERE ticket_id=?", (reused,)).fetchone()[0] == 0
                assert database.execute("SELECT id, report FROM artifact_verifications ORDER BY id").fetchall() == reports_before
            self.checks.append({"case": "reuse-rescores-remaining-ready-and-acquire-replays", "status": 200})
            cancelled = dict(body, reason="synthetic fixture cancellation before execution", **{"class": "user_cancellation", "evidence": {"fixture_cancelled_before_execution": True}})
            cancelled.pop("lease_ttl_seconds")
            headers["X-Voom-Idempotency-Key"] = "fixture-cancel-" + uuid.uuid4().hex
            assert self.forward(f'/v1/execution/lease/{outcome["lease_id"]}/fail', json.dumps(cancelled).encode(), headers)[0] == 200
            headers["X-Voom-Idempotency-Key"] = "fixture-idle-" + uuid.uuid4().hex
            idle = self.forward("/v1/execution/lease/acquire", encoded, headers)
            assert idle[0] == 200 and json.loads(idle[1])["data"]["outcome"] == "idle"
            idle_state = self.publication_state()
            assert self.forward("/v1/execution/lease/acquire", encoded, headers) == idle
            assert self.publication_state() == idle_state
            self.checks.append({"case": "post-reuse-idle-acquire-replays", "status": 200})
        finally:
            node_process.send_signal(signal.SIGCONT)

    def close(self):
        self.closing = True
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        assert not self.thread.is_alive(), "fixture proxy did not stop"

    def assert_complete(self):
        assert not self.failures, "group fixture failed: " + ",".join(self.failures)
        if self.mode in ("chain", "abort"):
            assert self.scalar_checked
            return
        assert self.completion_checked and self.group_checked and self.response_dropped and self.replayed
