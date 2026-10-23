# Scan dispatch repair plan (#731)

Branch: `feat/scan-dispatch-731`; base: `main`.
Expected implementation size: 65–110 changed lines (M).
Frozen denominator: 250 (M), live #731 scan-transport/lifecycle assessment.
Guardrails: focused `cargo test -p voom-node-agent candidate_dispatch_keys`,
then `just fmt-check`, `just check-test-layout`, final `just ci` before push.
No ADR/migration or mandatory shared edit. No deferrals.

## Task 1 — Preserve scan identity in HTTP-safe locator components
Files: `crates/voom-node-agent/src/scan_session.rs` and sibling
`scan_session_test.rs`. The pump already owns this identity; no ownership move.

Interfaces: consume existing `ScanPumpContext`, `ScanCandidate`,
`process_candidate(registry, root, candidate, context)` and BLAKE3 `hash(&[u8])`.
Keep `ClientHandle::dispatch(credentials, &str, OperationRequest)` unchanged.
Downstream hash/probe requests keep original locator payloads.

Verification inventory:
- Contract: primary/sidecar child keys are bounded ASCII, deterministic on retry,
  distinct across locators/roles/leases, and do not transform payload locators.
  Mode: focused-test. Extend the existing
  `candidate_dispatch_keys_isolate_scan_executions_and_preserve_retries` intent
  with `candidate_dispatch_keys_encode_unicode_and_separate_locator_components`.
  Expected red: a recorded key contains non-ASCII locator bytes, or delimiter-
  ambiguous locator pairs share a key. Green command:
  `cargo test -p voom-node-agent candidate_dispatch_keys`.
- Surface: explanatory spec/plan/evidence prose. Mode: task-test-not-applicable;
  no executable consumer validates these human-readable explanations.

Steps:
1. Add a sibling test using existing scripted hash/probe workers and registry.
   Run Unicode primary+sidecar, distinct delimiter-rich primary+sidecar pairs,
   and repeated same candidate under the same context. Inspect recorded keys and
   request bodies: ASCII bounded length, equal retry pair, distinct operation
   keys, exact original locator fields. Existing test covers different leases.
   Run the focused command and retain its failing assertion before production edit.

```rust
#[tokio::test]
async fn candidate_dispatch_keys_encode_unicode_and_separate_locator_components() {
    let incarnation = "0123456789abcdef0123456789abcdef".parse().unwrap();
    let context = pump_context(NodeId(9_000_001), incarnation);
    let hash = ScriptedSingleFileWorker::new(OperationKind::HashFile, SingleFileBehavior::Agree);
    let probe = ScriptedSingleFileWorker::new(OperationKind::ProbeFile, SingleFileBehavior::Agree);
    let registry = registry_with(
        ScriptedScanWorker::new(Vec::new()),
        hash.clone(),
        probe.clone(),
    );
    let long_locator = format!("{}.mkv", "路".repeat(200));
    for (primary, sidecar) in [
        ("é.mkv", "字幕.srt"),
        ("a-b", "c.srt"),
        ("a", "b-c.srt"),
        (long_locator.as_str(), "space and | delimiter.srt"),
        ("é.mkv", "字幕.srt"),
    ] {
        let contribution = super::process_candidate(
            registry.clone(),
            "/media".into(),
            sidecar_candidate(primary, sidecar),
            context.clone(),
        )
        .await
        .unwrap_or_else(|failure| unreachable!("candidate failed: {}", failure.reason));
        assert!(contribution.observation.unwrap().evidence.is_some());
    }
    let hashes = hash.dispatches.lock().await;
    let probes = probe.dispatches.lock().await;
    assert_eq!(hashes[0].1.payload["provider_relative_locator"], "é.mkv");
    assert_eq!(hashes[1].1.payload["provider_relative_locator"], "字幕.srt");
    assert_eq!(probes[0].1.payload["path"], "/media/é.mkv");
    let mut bodies = std::collections::BTreeMap::new();
    for (key, request) in hashes.iter().chain(probes.iter()) {
        assert!(key.is_ascii(), "valid locators must yield HTTP-text keys");
        assert!(key.len() <= 192, "locator length must not grow the key");
        let body = serde_json::to_vec(request).unwrap();
        if let Some(previous) = bodies.insert(key, body.clone()) {
            assert_eq!(previous, body, "distinct locator tuples must not alias");
        }
    }
    assert_eq!(bodies.len(), 12, "four distinct candidates, three operations");
    assert_eq!(&hashes[..2], &hashes[8..]);
    assert_eq!(probes[0], probes[4]);
}
```

2. In `process_candidate`, replace the primary tag construction with:
   ```rust
   let locator_digest = blake3::hash(locator.as_bytes());
   let tag = format!(
       "{}-{}-{}",
       context.incarnation_id, context.lease_id.0, locator_digest.to_hex()
   );
   ```
   Replace the sidecar tag construction with:
   ```rust
   let sidecar_digest = blake3::hash(sidecar.provider_relative_locator.as_bytes());
   let sidecar_tag = format!("{tag}-{}", sidecar_digest.to_hex());
   ```
   Preserve every request payload, suffix, guard and failure path.
3. Run focused tests green, formatting/layout guards, then the real synthetic
   stack with a Unicode primary and subtitle sidecar. Scan twice with the same
   live owning incarnation; assert successful session/ticket/lease/access-plan
   outcomes and unchanged input facts. The retained pre-repair rejected-dispatch
   case supplies the negative lifecycle proof; no unestablished settlement patch.
4. Run full `just ci`; independent branch/security review, then simplify only
   actual changed code. Commit with normal hooks. Publish reviewed PR closing
   #731 only; #339 remains open. Worker returns merge-ready without merging.

Rollback: revert the code/test commit; no migration or input cleanup. Retain
synthetic runtime evidence; no production/canary media mutation or deletion.
