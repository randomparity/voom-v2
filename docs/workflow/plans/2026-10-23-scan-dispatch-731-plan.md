# Scan dispatch repair plan (#731)

Branch: `feat/scan-dispatch-731`; base: `main`.
Expected implementation size: 190–280 changed lines (M).
Frozen denominator: 250 (M), live #731 scan-transport/lifecycle assessment.
Guardrails: focused `cargo test -p voom-node-agent candidate_dispatch_keys`,
then `just fmt-check`, `just check-test-layout`, final `just ci` before push.
No ADR/migration or mandatory shared edit. No deferrals.

The later operator request explicitly adds inventory path-diversity selection.
The two tasks remain bounded M; the new cycle preserves the mapped 250 denominator.

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

## Task 2 — Select interesting path shapes alongside media shapes
Files: `scripts/issue-339-media-inventory.py`, new sibling
`scripts/issue-339-media-inventory-selftest.py`, and `justfile` source-check recipe.
Interfaces: `path_interest_tags(relative_path: str) -> list[str]` consumes exact
root-relative locators. `summarize_probe` and `failed_record` add those tags;
`select_recommended` retains existing selection/profile contracts except feature-
distinct pathname duplicates now remain selectable. No mutation of filenames.

Verification inventory:
- Contract: path-only diversity affects actual recommendations in both modes,
  byte thresholds are inclusive, roots do not contribute features, failed probes
  retain features and incompatible files stay excluded from reference-user.
  Mode: focused-test. New `InventoryPathTests` exercises real record builders and
  `recommended_for_mode` with identical compatible codec payloads and temporary
  paths, including same-basename/size Unicode-parent alternatives. Expected red:
  ordinary media is chosen instead of a path variant, missing path tags/boundary,
  or dedup drops a feature-distinct file. Green command:
  `just media-inventory-selftest` (direct Python invocation is the initial red
  command before the recipe exists).
- Contract: CI invokes the selftest. Mode: focused-test. The existing workflow
  runs `just source-checks`; verify `just --show source-checks` includes the new
  recipe, then full `just ci` executes its six recommendation-focused cases.

Steps:
1. Add unittest fixtures with fake identical ffprobe payloads and real temporary
   files; test each path feature beats plain media under a one-file limit for
   both profiles. Test 199/200-byte components using multibyte names, 1023/1024
   relative bytes, successful/failed root-prefix isolation, compatibility and
   same-basename feature dedup. Run Python selftest red before script changes.
2. Add five path tags and priorities; implement deterministic root-relative
   classification and attach it in both record builders; include feature tuple
   in existing equivalent-media dedup. Add the recipe to source-checks. Run green.
3. Retain the unchanged Rust real-stack proof, run `just ci`, and include both
   surfaces in branch/security review. No full-library inventory is needed.

### Task 2 exact classifier and selftest

Classifier to add before `interest_tags`:
```python
def path_interest_tags(relative_path: str) -> list[str]:
    """Sample relative-locator hazards; thresholds are heuristics, not validity limits."""
    components = Path(relative_path).parts
    tags: list[str] = []
    if not relative_path.isascii():
        tags.append("path_non_ascii")
    if any(len(part.encode("utf-8", errors="surrogateescape")) >= 200 for part in components):
        tags.append("path_long_component")
    if len(relative_path.encode("utf-8", errors="surrogateescape")) >= 1024:
        tags.append("path_long_relative")
    if any(part.startswith("-") for part in components):
        tags.append("path_leading_dash")
    if any(ord(character) < 32 or ord(character) == 127 for character in relative_path):
        tags.append("path_control_character")
    return tags
```

Selftest file:
```python
#!/usr/bin/env python3
"""Prove path diversity reaches canary recommendations, without media tools."""

import argparse
import importlib.util
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "media_inventory", Path(__file__).with_name("issue-339-media-inventory.py")
)
inventory = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inventory)

PAYLOAD = {
    "format": {"format_name": "matroska"},
    "streams": [
        {"codec_type": "video", "codec_name": "hevc"},
        {"codec_type": "audio", "codec_name": "aac", "channels": 6, "tags": {"language": "eng"}},
        {"codec_type": "subtitle", "codec_name": "subrip", "disposition": {"forced": 1}},
    ],
}


class InventoryPathTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / "é-root"
        self.root.mkdir()

    def record(self, relative, failed=False):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"same-size")
        if failed:
            return inventory.failed_record(path, self.root, "error", "fixture failure")
        return inventory.summarize_probe(path, self.root, PAYLOAD)

    def selected(self, records, mode, limit=1):
        args = argparse.Namespace(selection_mode=mode, sample_per_tag=1, recommend_limit=limit)
        return [item["relative_path"] for item in inventory.recommended_for_mode(records, args)]

    def test_path_features_win_over_codec_identical_plain_files_in_both_modes(self):
        paths = [
            "é-parent/plain.mkv",
            "x" * 196 + ".mkv",
            "/".join(["d" * 64] * 16 + ["plain.mkv"]),
            "-directory/plain.mkv",
            "line\nbreak.mkv",
        ]
        plain = self.record("a/plain.mkv")
        for relative in paths:
            feature = self.record(relative)
            for mode in ["interesting", "reference-user"]:
                with self.subTest(path=relative, mode=mode):
                    self.assertEqual(self.selected([plain, feature], mode), [relative])

    def test_byte_thresholds_are_inclusive_and_not_character_counts(self):
        below = "é" * 97 + "x.mkv"  # 199 UTF-8 bytes.
        boundary = "é" * 98 + ".mkv"  # 200 UTF-8 bytes.
        self.assertNotIn("path_long_component", inventory.path_interest_tags(below))
        self.assertIn("path_long_component", inventory.path_interest_tags(boundary))
        relative = "/".join(["d" * 63] * 15) + "/" + "f" * 59 + ".mkv"
        self.assertEqual(len(relative.encode()), 1023)
        self.assertNotIn("path_long_relative", inventory.path_interest_tags(relative))
        self.assertIn("path_long_relative", inventory.path_interest_tags("f" + relative))

    def test_root_prefix_never_contributes_path_tags(self):
        for failed in [False, True]:
            record = self.record("plain.mkv", failed)
            self.assertFalse(any(tag.startswith("path_") for tag in record["interest_tags"]))

    def test_feature_distinct_same_basename_and_size_survives_media_dedup(self):
        plain = self.record("a/plain.mkv")
        unicode = self.record("é/plain.mkv")
        for mode in ["interesting", "reference-user"]:
            self.assertEqual(set(self.selected([plain, unicode], mode, 2)),
                             {"a/plain.mkv", "é/plain.mkv"})

    def test_failed_probe_keeps_path_features_without_becoming_policy_compatible(self):
        failed = self.record("é/-clip.mkv", True)
        self.assertIn("probe_failure", failed["interest_tags"])
        self.assertIn("path_non_ascii", failed["interest_tags"])
        self.assertIn("path_leading_dash", failed["interest_tags"])
        self.assertEqual(self.selected([failed], "interesting"), ["é/-clip.mkv"])
        self.assertEqual(self.selected([failed], "reference-user"), [])

    def test_path_features_do_not_override_reference_user_compatibility(self):
        compatible = self.record("plain.mkv")
        incompatible = self.record("é.mkv")
        incompatible["reference_user_current"]["compatible"] = False
        self.assertEqual(self.selected([incompatible, compatible], "reference-user"), ["plain.mkv"])


if __name__ == "__main__":
    unittest.main()
```

Record/selection edits: prepend the classifier tags in `interest_tags(record)`;
use `["probe_failure", *path_interest_tags(str(path.relative_to(root)))]` in
`failed_record`; define `PATH_TAG_PRIORITY` in the same classifier order and
expand it first in both existing priority arrays. Extend `selected_media` tuple
type and `media_key` with `tuple(path_interest_tags(record["relative_path"]))`.
Recipe: `media-inventory-selftest:`, body
`python3 scripts/issue-339-media-inventory-selftest.py`; include this recipe in
the existing `source-checks` dependency list.
