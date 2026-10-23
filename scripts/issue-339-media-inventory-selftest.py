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
            if len(relative.encode()) >= 1024:
                # Exercise the selector value without requiring a host to create
                # a path beyond its own filesystem limit.
                feature = dict(plain, path=str(self.root / relative), relative_path=relative)
                feature["interest_tags"] = inventory.interest_tags(feature)
            else:
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
