# VOOM Sprint 15 — Named Video Encode Profiles — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Sprint 12's single hardcoded HEVC profile with named, validated, durable video encode profiles (HEVC + AV1, software encoders) referenced by policy by name or inline, applied end-to-end through compiler, store, ffprobe projection, planner, control plane, FFmpeg worker, and CLI.

**Architecture:** A new `video_profiles` STRICT table (migration-seeded, read-only) holds the curated built-ins. `voom-worker-protocol` gains the full encode field set plus per-encoder capability descriptors (pure data + predicates). The compiler emits a typed `VideoProfileRef` (`Named|Inline`) validating inline settings against the descriptors; the control plane resolves `Named` refs against the registry at planning-input assembly into a fully-typed `TranscodeVideoProfile`. The planner consumes the resolved profile for dimension-/pixel-format-/profile-/level-/container-aware compliance and resource notes. The FFmpeg worker builds per-encoder command shapes, muxes MKV/MP4, downscales, and honors a control-plane-decided `copy_video` short-circuit. The worker never writes SQLite; the control plane owns resolution, the copy decision, artifact identity, verification, commit, and reporting.

**Tech Stack:** Rust (stable, workspace lints: pedantic, `unwrap/expect/panic` denied, `allow_attributes` denied — use `#[expect(..., reason = "...")]`), tokio + sqlx (SQLite, `STRICT` tables, `sqlx::migrate!`), serde / serde_json, blake3, clap, insta (CLI snapshots). All routine actions via `just` (`just ci`, `just test`, `just lint`, `cargo insta review`).

---

## Conventions (apply to every task)

- **Tests are sibling files.** Unit tests live in `<source>_test.rs` linked from the parent via `#[cfg(test)] #[path = "<source>_test.rs"] mod tests;`. No inline `#[cfg(test)] mod tests { ... }` in `src/`. Integration tests go under `crates/*/tests/`. `just check-test-layout` enforces this.
- **Lint suppression:** use `#[expect(clippy::..., reason = "...")]`, never `#[allow(...)]`.
- **Transactions:** post-update re-reads inside `_in_tx` go through `&mut **tx`, never `self.get()` / `self.pool`.
- **No relative imports.** Absolute paths only (`voom_worker_protocol::...`, `crate::...`).
- **CLI output:** exactly one JSON envelope on stdout via `envelope::emit_ok` / `emit_err`; logs to stderr.
- **Error codes** are public contract (`voom_core::VoomError::code()`); add variants, never rename.
- **Commit cadence:** one logical change per commit, imperative subject ≤72 chars. End commit messages with the `Co-Authored-By` trailer. Work stays on `feat/sprint-15`; do **not** open a PR until the whole sprint lands (sprint-iteration convention).
- **Per phase:** finish a phase with `just ci` green before starting the next.

## File Structure (created / modified across all phases)

**Phase 1 — worker-protocol**
- Modify: `crates/voom-worker-protocol/src/transcode_video.rs` — extend `TranscodeVideoProfile`, `TranscodeVideoRequest` (`copy_video`), `TranscodeVideoResult` (`output_width/height/pixel_format`, `copied_video`); codec/container enums.
- Create: `crates/voom-worker-protocol/src/encoder_caps.rs` — per-encoder capability descriptors + predicates.
- Create: `crates/voom-worker-protocol/src/encoder_caps_test.rs`.
- Modify: `crates/voom-worker-protocol/src/lib.rs` — `pub mod encoder_caps;` + re-exports.
- Modify: `crates/voom-worker-protocol/src/transcode_video_test.rs` — update golden fixtures, add `copy_video`/result-field tests.

**Phase 2 — store**
- Create: `migrations/0014_video_profiles.sql`.
- Modify: `crates/voom-store/src/migrator.rs` — register migration 14.
- Create: `crates/voom-store/src/repo/video_profiles.rs` (+ `_test.rs`).
- Modify: `crates/voom-store/src/repo/mod.rs` — module + re-exports.

**Phase 3 — policy**
- Create: `crates/voom-policy/src/video_profile.rs` — `VideoProfileRef`, `VideoProfileSettings` (+ `_test.rs`).
- Modify: `crates/voom-policy/src/ast.rs` — transcode block carries `Vec<SettingAst>`.
- Modify: `crates/voom-policy/src/parser.rs` — parse transcode inline `{ key: value }` body as settings.
- Modify: `crates/voom-policy/src/validate.rs` — generalize `validate_transcode_statement`.
- Modify: `crates/voom-policy/src/compiled.rs` — `TranscodeVideo.profile: VideoProfileRef` + `resolved_profile` field, `statement_text()` `TranscodeInline` arm, lower hevc/av1 + using-profile + inline.
- Modify: `crates/voom-policy/src/lib.rs` — re-exports.
- Modify test files: `validate_test.rs`, `compiled_test.rs`, `parser_test.rs`, `pipeline_test.rs`.
- Modify (cross-crate compile-fix for the variant change, see Task 3.4): `crates/voom-plan/src/planner.rs`, `crates/voom-plan/src/planner_test.rs`.

**Phase 4 — ffprobe / projection**
- Modify: `crates/voom-ffprobe-worker/src/normalize.rs` — capture `pixel_format`, `profile`, `level`; bump snapshot format token.
- Modify: `crates/voom-ffprobe-worker/src/normalize_test.rs`.
- Modify: `crates/voom-control-plane/src/media_snapshot.rs` — project `width`/`height`/video stream pixel-format/profile/level.
- Modify: `crates/voom-control-plane/src/media_snapshot_test.rs`.

**Phase 5 — planner**
- Modify: `crates/voom-plan/src/planner.rs` — profile-aware `transcode_video_shape`, MP4 gating, `ResourceEstimates` notes.
- Create: `crates/voom-plan/src/transcode_video_profile.rs` — resolved-profile view + `inline-<hash>` + cpu-cost lookup (+ `_test.rs`).
- Modify: `crates/voom-plan/src/lib.rs`, `crates/voom-plan/src/planner_test.rs`.

**Phase 6 — control-plane**
- Create: `crates/voom-control-plane/src/transcode/resolve.rs` — `Named`→typed resolution, `copy_video` decision (+ `_test.rs`).
- Modify: `crates/voom-control-plane/src/cases/compliance.rs` — resolve before planning.
- Modify: `crates/voom-control-plane/src/transcode/dispatch.rs` — consume resolved profile + `copy_video`.
- Modify: `crates/voom-control-plane/src/transcode/stage.rs` — profile-identity target naming.
- Modify: `crates/voom-control-plane/src/transcode/mod.rs`, `commit.rs`, `events.rs`; `lib.rs` (repo field).
- Modify: `crates/voom-control-plane/src/workflow/binding.rs` — thread `VideoProfileRef`-shaped payload.

**Phase 7 — ffmpeg worker**
- Modify: `crates/voom-ffmpeg-worker/src/ffmpeg.rs` — per-encoder command shapes, MP4 mux, downscale, `-c:v copy`, output validation (dims/pixfmt).
- Modify: `crates/voom-ffmpeg-worker/src/handler.rs` — copy-precondition revalidation, contract checks.
- Modify: `crates/voom-ffmpeg-worker/src/preflight.rs` — per-encoder availability.
- Modify test files: `ffmpeg_test.rs`, `handler_test.rs`, `preflight_test.rs`.

**Phase 8 — CLI + integration + closeout**
- Modify: `crates/voom-cli/src/cli.rs`, `src/main.rs`, `src/commands/mod.rs`.
- Create: `crates/voom-cli/src/commands/profile.rs` (+ snapshots).
- Modify: transcode report data structs to carry profile facts.
- Create: `crates/voom-control-plane/tests/video_profile_flow.rs` (integration).
- Create: `docs/superpowers/specs/2026-05-28-voom-sprint-15-closeout.md`.

---

# Phase 1 — Worker Protocol: Model + Capability Descriptors

**Foundation.** Everything downstream depends on the extended `TranscodeVideoProfile`, the `copy_video` request flag, the extended result, and the per-encoder capability descriptors (pure data + predicates) that the compiler (Phase 3) and store seed validation (Phase 2) call.

**Design notes baked into this phase:**
- New optional profile fields use `#[serde(skip_serializing_if = "Option::is_none")]`; `copy_video` and `copy_compatible` skip when `false`. A `default_hevc()` payload therefore serializes to a minimal superset of the Sprint 12 shape — the FFmpeg command line is unchanged — but it now also serializes the newly-required `target_codec` key, so Sprint 12 golden fixtures are **updated**, not preserved verbatim. Tests assert command-line invariance, not byte-identical JSON.
- `target_codec` and `encoder` are validated against finite allowlists. The model is structured (a per-encoder descriptor keyed by encoder string) so new encoders/codecs slot in without reshaping.

### Task 1.1: Codec + container enums and helpers

**Files:**
- Modify: `crates/voom-worker-protocol/src/transcode_video.rs`
- Modify: `crates/voom-worker-protocol/src/transcode_video_test.rs`

- [ ] **Step 1: Write the failing test** — append to `transcode_video_test.rs`:

```rust
#[test]
fn supported_codecs_and_containers_are_recognized() {
    assert!(is_supported_transcode_video_codec("hevc"));
    assert!(is_supported_transcode_video_codec("H265")); // alias, case-insensitive
    assert!(is_supported_transcode_video_codec("av1"));
    assert!(is_supported_transcode_video_codec("AV1"));
    assert!(!is_supported_transcode_video_codec("h264"));
    assert!(is_supported_transcode_video_container("mkv"));
    assert!(is_supported_transcode_video_container("mp4"));
    assert!(!is_supported_transcode_video_container("avi"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-worker-protocol supported_codecs_and_containers -- --nocapture`
Expected: FAIL — `av1` not recognized, `mp4` not recognized.

- [ ] **Step 3: Implement** — extend the top of `transcode_video.rs`:

```rust
pub const TRANSCODE_VIDEO_CONTAINER: &str = "mkv";
pub const TRANSCODE_VIDEO_CONTAINER_MP4: &str = "mp4";
pub const TRANSCODE_VIDEO_CODEC: &str = "hevc";
pub const TRANSCODE_VIDEO_CODEC_ALIAS_H265: &str = "h265";
pub const TRANSCODE_VIDEO_CODEC_AV1: &str = "av1";
pub const TRANSCODE_VIDEO_PROFILE: &str = "default-hevc";

#[must_use]
pub fn is_supported_transcode_video_container(container: &str) -> bool {
    container == TRANSCODE_VIDEO_CONTAINER || container == TRANSCODE_VIDEO_CONTAINER_MP4
}

#[must_use]
pub fn is_supported_transcode_video_codec(codec: &str) -> bool {
    codec.eq_ignore_ascii_case(TRANSCODE_VIDEO_CODEC)
        || codec.eq_ignore_ascii_case(TRANSCODE_VIDEO_CODEC_ALIAS_H265)
        || codec.eq_ignore_ascii_case(TRANSCODE_VIDEO_CODEC_AV1)
}

/// Returns the canonical codec token (`"hevc"` or `"av1"`) for a recognized
/// codec name or alias, or `None` when unrecognized.
#[must_use]
pub fn canonical_video_codec(codec: &str) -> Option<&'static str> {
    if codec.eq_ignore_ascii_case(TRANSCODE_VIDEO_CODEC)
        || codec.eq_ignore_ascii_case(TRANSCODE_VIDEO_CODEC_ALIAS_H265)
    {
        Some(TRANSCODE_VIDEO_CODEC)
    } else if codec.eq_ignore_ascii_case(TRANSCODE_VIDEO_CODEC_AV1) {
        Some(TRANSCODE_VIDEO_CODEC_AV1)
    } else {
        None
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p voom-worker-protocol supported_codecs_and_containers`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-worker-protocol/src/transcode_video.rs crates/voom-worker-protocol/src/transcode_video_test.rs
git commit -m "feat(protocol): recognize av1 codec and mp4 container"
```

### Task 1.2: Per-encoder capability descriptors

The descriptors are the single source of truth for what each encoder accepts. `libx265` (HEVC) uses named presets and CRF 0–51; `libsvtav1` (AV1) uses numeric `-preset 0–13` and CRF 0–63; `libaom-av1` (AV1) uses numeric `-cpu-used 0–8`, CRF 0–63, and requires `-b:v 0`.

**Files:**
- Create: `crates/voom-worker-protocol/src/encoder_caps.rs`
- Create: `crates/voom-worker-protocol/src/encoder_caps_test.rs`
- Modify: `crates/voom-worker-protocol/src/lib.rs`

- [ ] **Step 1: Write the failing test** — create `encoder_caps_test.rs`:

```rust
use super::*;

#[test]
fn descriptor_lookup_is_keyed_on_encoder() {
    assert!(encoder_descriptor("libx265").is_some());
    assert!(encoder_descriptor("libsvtav1").is_some());
    assert!(encoder_descriptor("libaom-av1").is_some());
    assert!(encoder_descriptor("x264").is_none());
}

#[test]
fn encoder_must_match_target_codec() {
    let x265 = encoder_descriptor("libx265").unwrap();
    assert_eq!(x265.target_codec, "hevc");
    let svt = encoder_descriptor("libsvtav1").unwrap();
    assert_eq!(svt.target_codec, "av1");
}

#[test]
fn crf_range_is_per_encoder() {
    let x265 = encoder_descriptor("libx265").unwrap();
    assert!(x265.accepts_crf(23));
    assert!(x265.accepts_crf(51));
    assert!(!x265.accepts_crf(52));
    let svt = encoder_descriptor("libsvtav1").unwrap();
    assert!(svt.accepts_crf(63));
    assert!(!svt.accepts_crf(64));
}

#[test]
fn preset_domain_is_named_for_x265_numeric_for_av1() {
    let x265 = encoder_descriptor("libx265").unwrap();
    assert!(x265.accepts_preset("medium"));
    assert!(x265.accepts_preset("placebo"));
    assert!(!x265.accepts_preset("8"));
    let svt = encoder_descriptor("libsvtav1").unwrap();
    assert!(svt.accepts_preset("8"));
    assert!(svt.accepts_preset("0"));
    assert!(svt.accepts_preset("13"));
    assert!(!svt.accepts_preset("14"));
    assert!(!svt.accepts_preset("medium"));
    let aom = encoder_descriptor("libaom-av1").unwrap();
    assert!(aom.accepts_preset("4"));
    assert!(aom.accepts_preset("8"));
    assert!(!aom.accepts_preset("9"));
}

#[test]
fn pixel_format_and_profile_combinations_are_validated() {
    let x265 = encoder_descriptor("libx265").unwrap();
    assert!(x265.accepts_pixel_format("yuv420p"));
    assert!(x265.accepts_pixel_format("yuv420p10le"));
    assert!(!x265.accepts_pixel_format("rgb24"));
    assert!(x265.accepts_codec_profile("main10"));
    // 10-bit pixel format under an 8-bit-only codec profile is incompatible.
    assert!(x265.pixel_format_compatible_with_profile("yuv420p10le", Some("main10")));
    assert!(!x265.pixel_format_compatible_with_profile("yuv420p10le", Some("main")));
    assert!(x265.pixel_format_compatible_with_profile("yuv420p", Some("main")));
}

#[test]
fn libaom_requires_constant_quality_bitrate_zero() {
    let aom = encoder_descriptor("libaom-av1").unwrap();
    assert!(aom.requires_bitrate_zero);
    let svt = encoder_descriptor("libsvtav1").unwrap();
    assert!(!svt.requires_bitrate_zero);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-worker-protocol encoder_caps`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Implement** — create `encoder_caps.rs`. Use static slices keyed by encoder string; predicates are pure functions:

```rust
//! Per-encoder capability descriptors: the finite, encoder-specific vocabulary
//! for CRF, preset, tune, codec profile/level, and pixel format. Pure data and
//! predicates shared by the policy compiler (inline validation), the store seed
//! validation, the planner (resource notes), and the FFmpeg worker.

/// How an encoder spells its speed knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetDomain {
    /// A fixed named set, e.g. x265 `ultrafast..placebo`.
    Named(&'static [&'static str]),
    /// An inclusive numeric range, e.g. SVT-AV1 `-preset 0..=13`.
    NumericRange { min: u8, max: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderDescriptor {
    pub encoder: &'static str,
    pub target_codec: &'static str,
    pub crf_min: u8,
    pub crf_max: u8,
    pub preset_domain: PresetDomain,
    pub tunes: &'static [&'static str],
    pub codec_profiles: &'static [&'static str],
    pub codec_levels: &'static [&'static str],
    pub pixel_formats: &'static [&'static str],
    /// 10-bit pixel formats for this encoder (subset of `pixel_formats`).
    pub ten_bit_pixel_formats: &'static [&'static str],
    /// Codec profiles that only allow 8-bit pixel formats.
    pub eight_bit_only_profiles: &'static [&'static str],
    /// `libaom-av1` constant-quality mode requires `-b:v 0`.
    pub requires_bitrate_zero: bool,
}

const X265_PRESETS: &[&str] = &[
    "ultrafast", "superfast", "veryfast", "faster", "fast", "medium", "slow", "slower",
    "veryslow", "placebo",
];

const X265: EncoderDescriptor = EncoderDescriptor {
    encoder: "libx265",
    target_codec: "hevc",
    crf_min: 0,
    crf_max: 51,
    preset_domain: PresetDomain::Named(X265_PRESETS),
    tunes: &["psnr", "ssim", "grain", "fastdecode", "zerolatency"],
    codec_profiles: &["main", "main10", "main12"],
    codec_levels: &["3.0", "3.1", "4.0", "4.1", "5.0", "5.1", "5.2", "6.0", "6.1", "6.2"],
    pixel_formats: &["yuv420p", "yuv420p10le", "yuv422p", "yuv422p10le", "yuv444p", "yuv444p10le"],
    ten_bit_pixel_formats: &["yuv420p10le", "yuv422p10le", "yuv444p10le"],
    eight_bit_only_profiles: &["main"],
    requires_bitrate_zero: false,
};

const SVTAV1: EncoderDescriptor = EncoderDescriptor {
    encoder: "libsvtav1",
    target_codec: "av1",
    crf_min: 0,
    crf_max: 63,
    preset_domain: PresetDomain::NumericRange { min: 0, max: 13 },
    tunes: &["vq", "psnr"],
    codec_profiles: &["main"],
    codec_levels: &["4.0", "4.1", "5.0", "5.1", "6.0", "6.1"],
    pixel_formats: &["yuv420p", "yuv420p10le"],
    ten_bit_pixel_formats: &["yuv420p10le"],
    eight_bit_only_profiles: &[],
    requires_bitrate_zero: false,
};

const LIBAOM: EncoderDescriptor = EncoderDescriptor {
    encoder: "libaom-av1",
    target_codec: "av1",
    crf_min: 0,
    crf_max: 63,
    preset_domain: PresetDomain::NumericRange { min: 0, max: 8 },
    tunes: &["psnr", "ssim"],
    codec_profiles: &["main", "high", "professional"],
    codec_levels: &["4.0", "4.1", "5.0", "5.1", "6.0", "6.1"],
    pixel_formats: &["yuv420p", "yuv420p10le"],
    ten_bit_pixel_formats: &["yuv420p10le"],
    eight_bit_only_profiles: &[],
    requires_bitrate_zero: true,
};

const DESCRIPTORS: &[EncoderDescriptor] = &[X265, SVTAV1, LIBAOM];

#[must_use]
pub fn encoder_descriptor(encoder: &str) -> Option<&'static EncoderDescriptor> {
    DESCRIPTORS.iter().find(|d| d.encoder == encoder)
}

#[must_use]
pub fn all_encoder_descriptors() -> &'static [EncoderDescriptor] {
    DESCRIPTORS
}

impl EncoderDescriptor {
    #[must_use]
    pub const fn accepts_crf(&self, crf: u8) -> bool {
        crf >= self.crf_min && crf <= self.crf_max
    }

    #[must_use]
    pub fn accepts_preset(&self, preset: &str) -> bool {
        match self.preset_domain {
            PresetDomain::Named(set) => set.contains(&preset),
            PresetDomain::NumericRange { min, max } => preset
                .parse::<u8>()
                .is_ok_and(|value| value >= min && value <= max),
        }
    }

    #[must_use]
    pub fn accepts_tune(&self, tune: &str) -> bool {
        self.tunes.contains(&tune)
    }

    #[must_use]
    pub fn accepts_codec_profile(&self, profile: &str) -> bool {
        self.codec_profiles.contains(&profile)
    }

    #[must_use]
    pub fn accepts_codec_level(&self, level: &str) -> bool {
        self.codec_levels.contains(&level)
    }

    #[must_use]
    pub fn accepts_pixel_format(&self, pixel_format: &str) -> bool {
        self.pixel_formats.contains(&pixel_format)
    }

    /// A 10-bit pixel format is incompatible with an 8-bit-only codec profile.
    #[must_use]
    pub fn pixel_format_compatible_with_profile(
        &self,
        pixel_format: &str,
        codec_profile: Option<&str>,
    ) -> bool {
        let Some(profile) = codec_profile else {
            return true;
        };
        if !self.eight_bit_only_profiles.contains(&profile) {
            return true;
        }
        !self.ten_bit_pixel_formats.contains(&pixel_format)
    }
}

#[cfg(test)]
#[path = "encoder_caps_test.rs"]
mod tests;
```

- [ ] **Step 4: Wire the module** — in `crates/voom-worker-protocol/src/lib.rs` add `pub mod encoder_caps;` near the other `pub mod` lines and add to the re-export block:

```rust
pub use encoder_caps::{
    EncoderDescriptor, PresetDomain, all_encoder_descriptors, encoder_descriptor,
};
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p voom-worker-protocol encoder_caps`
Expected: PASS (all 6 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/voom-worker-protocol/src/encoder_caps.rs crates/voom-worker-protocol/src/encoder_caps_test.rs crates/voom-worker-protocol/src/lib.rs
git commit -m "feat(protocol): add per-encoder capability descriptors"
```

### Task 1.3: Extend `TranscodeVideoProfile`, request, and result

**Files:**
- Modify: `crates/voom-worker-protocol/src/transcode_video.rs`
- Modify: `crates/voom-worker-protocol/src/transcode_video_test.rs`

- [ ] **Step 1: Write the failing test** — replace/extend the existing serialization tests so they assert the new minimal shape. Add:

```rust
#[test]
fn default_hevc_profile_serializes_minimal_superset() {
    let profile = TranscodeVideoProfile::default_hevc();
    let value = serde_json::to_value(&profile).unwrap();
    // Required keys present.
    assert_eq!(value["name"], "default-hevc");
    assert_eq!(value["target_codec"], "hevc");
    assert_eq!(value["encoder"], "libx265");
    assert_eq!(value["crf"], 23);
    assert_eq!(value["preset"], "medium");
    // All optional keys omitted; copy_compatible (false) omitted.
    let obj = value.as_object().unwrap();
    assert!(!obj.contains_key("tune"));
    assert!(!obj.contains_key("codec_profile"));
    assert!(!obj.contains_key("codec_level"));
    assert!(!obj.contains_key("pixel_format"));
    assert!(!obj.contains_key("max_width"));
    assert!(!obj.contains_key("max_height"));
    assert!(!obj.contains_key("copy_compatible"));
    assert_eq!(obj.len(), 5);
}

#[test]
fn request_carries_copy_video_flag_skipped_when_false() {
    let req = sample_request(); // helper below, copy_video defaults false
    let value = serde_json::to_value(&req).unwrap();
    assert!(!value.as_object().unwrap().contains_key("copy_video"));

    let mut req_copy = sample_request();
    req_copy.copy_video = true;
    let value = serde_json::to_value(&req_copy).unwrap();
    assert_eq!(value["copy_video"], true);
}

#[test]
fn result_carries_observed_output_dimensions_and_copied_flag() {
    let json = serde_json::json!({
        "status": "transcoded",
        "provider": "ffmpeg",
        "provider_version": "ffmpeg version 7.0",
        "input_pre": {"size_bytes": 1, "content_hash": "blake3:a"},
        "input_post": {"size_bytes": 1, "content_hash": "blake3:a"},
        "output": {"size_bytes": 2, "content_hash": "blake3:b"},
        "output_container": "mp4",
        "output_video_codec": "av1",
        "output_width": 1920,
        "output_height": 1080,
        "output_pixel_format": "yuv420p",
        "copied_video": false
    });
    let result: TranscodeVideoResult = serde_json::from_value(json).unwrap();
    assert_eq!(result.output_width, 1920);
    assert_eq!(result.output_height, 1080);
    assert_eq!(result.output_pixel_format, "yuv420p");
    assert!(!result.copied_video);
}
```

Add a `sample_request()` helper in the test file that builds a `TranscodeVideoRequest` with `default_hevc()` and `copy_video: false`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-worker-protocol transcode_video`
Expected: FAIL — fields don't exist; existing golden test also fails (expected — we update it next).

- [ ] **Step 3: Implement** — replace the profile/request/result structs in `transcode_video.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeVideoProfile {
    pub name: String,
    pub target_codec: String,
    pub encoder: String,
    pub crf: u8,
    pub preset: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tune: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixel_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub copy_compatible: bool,
}

#[expect(clippy::trivially_copy_pass_by_ref, reason = "serde skip_serializing_if signature")]
fn is_false(value: &bool) -> bool {
    !*value
}

impl TranscodeVideoProfile {
    #[must_use]
    pub fn default_hevc() -> Self {
        Self {
            name: TRANSCODE_VIDEO_PROFILE.to_owned(),
            target_codec: TRANSCODE_VIDEO_CODEC.to_owned(),
            encoder: "libx265".to_owned(),
            crf: 23,
            preset: "medium".to_owned(),
            tune: None,
            codec_profile: None,
            codec_level: None,
            pixel_format: None,
            max_width: None,
            max_height: None,
            copy_compatible: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeVideoRequest {
    pub input: TranscodeVideoInput,
    pub output: TranscodeVideoOutput,
    pub profile: TranscodeVideoProfile,
    #[serde(default, skip_serializing_if = "is_false")]
    pub copy_video: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscodeVideoResult {
    pub status: TranscodeVideoStatus,
    pub provider: String,
    pub provider_version: String,
    pub input_pre: TranscodeVideoObservedFacts,
    pub input_post: TranscodeVideoObservedFacts,
    pub output: TranscodeVideoObservedFacts,
    pub output_container: String,
    pub output_video_codec: String,
    pub output_width: u32,
    pub output_height: u32,
    pub output_pixel_format: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub copied_video: bool,
}
```

- [ ] **Step 4: Update the existing golden fixture test** — in `transcode_video_test.rs`, update `transcode_video_request_serializes_stable_snake_case_shape` to expect the new `target_codec` key and `output_width/height/pixel_format/copied_video` on the result. Update any inline JSON fixtures to include the now-required result fields.

- [ ] **Step 5: Run to verify all pass**

Run: `cargo test -p voom-worker-protocol`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-worker-protocol/src/transcode_video.rs crates/voom-worker-protocol/src/transcode_video_test.rs
git commit -m "feat(protocol): extend transcode video profile, request, and result fields"
```

### Task 1.4: Profile-validates-against-descriptor predicate

A reusable predicate the store seed test (Phase 2) and resolver (Phase 6) call to assert a fully-typed profile is internally consistent.

**Files:**
- Modify: `crates/voom-worker-protocol/src/transcode_video.rs`
- Modify: `crates/voom-worker-protocol/src/transcode_video_test.rs`

- [ ] **Step 1: Write the failing test**:

```rust
#[test]
fn profile_validates_against_its_encoder_descriptor() {
    let ok = TranscodeVideoProfile::default_hevc();
    assert!(validate_profile_against_descriptor(&ok).is_ok());

    let mut bad_codec = TranscodeVideoProfile::default_hevc();
    bad_codec.target_codec = "av1".to_owned(); // libx265 is hevc-only
    assert!(validate_profile_against_descriptor(&bad_codec).is_err());

    let mut bad_crf = TranscodeVideoProfile::default_hevc();
    bad_crf.crf = 60; // > 51 for libx265
    assert!(validate_profile_against_descriptor(&bad_crf).is_err());

    let mut bad_combo = TranscodeVideoProfile::default_hevc();
    bad_combo.pixel_format = Some("yuv420p10le".to_owned());
    bad_combo.codec_profile = Some("main".to_owned()); // 10-bit under 8-bit profile
    assert!(validate_profile_against_descriptor(&bad_combo).is_err());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-worker-protocol profile_validates_against`
Expected: FAIL — function missing.

- [ ] **Step 3: Implement** — add to `transcode_video.rs`:

```rust
use crate::encoder_caps::encoder_descriptor;

/// Validates a fully-typed profile against its encoder's capability descriptor.
/// Returns a stable, human-readable reason string on the first violation.
///
/// # Errors
/// Returns `Err` when the encoder is unknown, the target codec disagrees with
/// the encoder, or any field falls outside the encoder's vocabulary/range.
pub fn validate_profile_against_descriptor(
    profile: &TranscodeVideoProfile,
) -> Result<(), String> {
    let Some(descriptor) = encoder_descriptor(&profile.encoder) else {
        return Err(format!("unknown encoder `{}`", profile.encoder));
    };
    if descriptor.target_codec != profile.target_codec {
        return Err(format!(
            "encoder `{}` produces `{}`, not `{}`",
            profile.encoder, descriptor.target_codec, profile.target_codec
        ));
    }
    if !descriptor.accepts_crf(profile.crf) {
        return Err(format!(
            "crf {} outside {}..={} for `{}`",
            profile.crf, descriptor.crf_min, descriptor.crf_max, profile.encoder
        ));
    }
    if !descriptor.accepts_preset(&profile.preset) {
        return Err(format!("preset `{}` invalid for `{}`", profile.preset, profile.encoder));
    }
    if let Some(tune) = &profile.tune {
        if !descriptor.accepts_tune(tune) {
            return Err(format!("tune `{tune}` invalid for `{}`", profile.encoder));
        }
    }
    if let Some(codec_profile) = &profile.codec_profile {
        if !descriptor.accepts_codec_profile(codec_profile) {
            return Err(format!(
                "codec_profile `{codec_profile}` invalid for `{}`",
                profile.encoder
            ));
        }
    }
    if let Some(level) = &profile.codec_level {
        if !descriptor.accepts_codec_level(level) {
            return Err(format!("codec_level `{level}` invalid for `{}`", profile.encoder));
        }
    }
    if let Some(pixel_format) = &profile.pixel_format {
        if !descriptor.accepts_pixel_format(pixel_format) {
            return Err(format!(
                "pixel_format `{pixel_format}` invalid for `{}`",
                profile.encoder
            ));
        }
        if !descriptor
            .pixel_format_compatible_with_profile(pixel_format, profile.codec_profile.as_deref())
        {
            return Err(format!(
                "pixel_format `{pixel_format}` incompatible with codec_profile `{:?}`",
                profile.codec_profile
            ));
        }
    }
    Ok(())
}
```

Add `validate_profile_against_descriptor` to the `lib.rs` re-export from `transcode_video`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p voom-worker-protocol`
Expected: PASS.

- [ ] **Step 5: Phase gate — full CI**

Run: `just ci`
Expected: PASS. Fix any clippy/fmt issues before committing.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-worker-protocol/src/transcode_video.rs crates/voom-worker-protocol/src/transcode_video_test.rs crates/voom-worker-protocol/src/lib.rs
git commit -m "feat(protocol): validate resolved profile against encoder descriptor"
```

---

# Phase 2 — Store: `video_profiles` Migration + Repository

Depends on Phase 1 (descriptors, `TranscodeVideoProfile`, `validate_profile_against_descriptor`). Adds migration `0014_video_profiles.sql` (STRICT, seeded, read-only) and a read-only `SqliteVideoProfileRepo` (lookup-by-name, list). **This is the first migration in the repo that seeds rows** — the table is created and populated in the same migration.

### Task 2.1: Migration with STRICT table + seeded built-ins

**Files:**
- Create: `migrations/0014_video_profiles.sql`
- Modify: `crates/voom-store/src/migrator.rs`

- [ ] **Step 1: Write the migration** — create `migrations/0014_video_profiles.sql` (the table DDL is the spec's §4 schema verbatim; the six seed rows are §4's table). Use deterministic `id` strings.

> **Do NOT wrap the migration in `BEGIN;`/`COMMIT;`.** The `MIGRATOR` runs with `no_tx: false` (`crates/voom-store/src/migrator.rs`), so sqlx already wraps every migration in its own transaction. Migrations 0012/0013 use a `COMMIT;`-at-top / `BEGIN;`-at-bottom trick *only* to escape that wrapper for `PRAGMA foreign_keys = OFF` table-rebuild work — see 0012's header comment. A plain `CREATE TABLE` + `INSERT` (like migrations 0001–0009) needs no transaction control; embedding `BEGIN;`/`COMMIT;` here would commit the wrapper mid-migration and break `init`.

```sql
CREATE TABLE video_profiles (
  id               TEXT PRIMARY KEY,
  name             TEXT NOT NULL UNIQUE,
  target_codec     TEXT NOT NULL,
  encoder          TEXT NOT NULL,
  crf              INTEGER NOT NULL,
  preset           TEXT NOT NULL,
  tune             TEXT,
  codec_profile    TEXT,
  codec_level      TEXT,
  pixel_format     TEXT,
  max_width        INTEGER,
  max_height       INTEGER,
  output_container TEXT NOT NULL DEFAULT 'mkv',
  copy_compatible  INTEGER NOT NULL DEFAULT 0,
  CHECK (length(trim(name)) > 0),
  CHECK (target_codec IN ('hevc', 'av1')),
  CHECK (encoder IN ('libx265', 'libsvtav1', 'libaom-av1')),
  CHECK (crf >= 0),
  CHECK (max_width IS NULL OR max_width > 0),
  CHECK (max_height IS NULL OR max_height > 0),
  CHECK (output_container IN ('mkv', 'mp4')),
  CHECK (copy_compatible IN (0, 1))
) STRICT;

INSERT INTO video_profiles
  (id, name, target_codec, encoder, crf, preset, codec_profile, pixel_format,
   max_width, max_height, output_container, copy_compatible)
VALUES
  ('vp-default-hevc', 'default-hevc', 'hevc', 'libx265', 23, 'medium',
   NULL, NULL, NULL, NULL, 'mkv', 0),
  ('vp-hevc-archive', 'hevc-archive', 'hevc', 'libx265', 18, 'slow',
   'main10', 'yuv420p10le', NULL, NULL, 'mkv', 0),
  ('vp-hevc-1080p', 'hevc-1080p', 'hevc', 'libx265', 23, 'medium',
   NULL, NULL, 1920, 1080, 'mp4', 1),
  ('vp-default-av1', 'default-av1', 'av1', 'libsvtav1', 30, '8',
   NULL, NULL, NULL, NULL, 'mkv', 0),
  ('vp-av1-archive', 'av1-archive', 'av1', 'libaom-av1', 20, '4',
   NULL, NULL, NULL, NULL, 'mkv', 0),
  ('vp-av1-1080p', 'av1-1080p', 'av1', 'libsvtav1', 32, '8',
   NULL, NULL, 1920, 1080, 'mp4', 1);
```

- [ ] **Step 2: Register the migration** — in `crates/voom-store/src/migrator.rs`, add the `include_str!` const next to the others and append to the `MIGRATOR` vector:

```rust
const MIGRATION_0014_SQL: &str = include_str!("../../../migrations/0014_video_profiles.sql");
```

```rust
Migration::new(
    14,
    Cow::Borrowed("video_profiles"),
    MigrationType::Simple,
    Cow::Borrowed(MIGRATION_0014_SQL),
    false,
),
```

- [ ] **Step 3: Write the failing test** — create the repo test file first only after Task 2.2 exists; for the migration itself, add to `crates/voom-store/src/migrator.rs`'s sibling test (or a new `tests/` migration test) a check that a fresh init produces 6 rows and rejects a bad insert. Put it in `crates/voom-store/src/repo/video_profiles_test.rs` in Task 2.2.

- [ ] **Step 4: Run migration smoke**

Run: `just smoke`
Expected: PASS — `init` applies migration 14 without error.

- [ ] **Step 5: Commit**

```bash
git add migrations/0014_video_profiles.sql crates/voom-store/src/migrator.rs
git commit -m "feat(store): add seeded video_profiles migration"
```

### Task 2.2: `SqliteVideoProfileRepo` (lookup-by-name, list)

Mirror `SqlitePolicyRepo`: a domain struct, a `VideoProfileRepo: Repository` trait with `get_by_name` + `list`, a `SqliteVideoProfileRepo { pool }`, and a `row_to_video_profile` mapper.

**Files:**
- Create: `crates/voom-store/src/repo/video_profiles.rs`
- Create: `crates/voom-store/src/repo/video_profiles_test.rs`
- Modify: `crates/voom-store/src/repo/mod.rs`

- [ ] **Step 1: Write the failing test** — create `video_profiles_test.rs`:

```rust
use super::*;
use voom_store::test_support::fresh_initialized_pool_at;

async fn repo() -> (SqliteVideoProfileRepo, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (SqliteVideoProfileRepo::new(pool), tmp)
}

#[tokio::test]
async fn lists_all_seeded_builtins() {
    let (repo, _tmp) = repo().await;
    let profiles = repo.list().await.unwrap();
    let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"default-hevc"));
    assert!(names.contains(&"av1-1080p"));
    assert_eq!(profiles.len(), 6);
}

#[tokio::test]
async fn every_seeded_builtin_is_valid_against_its_descriptor() {
    let (repo, _tmp) = repo().await;
    for profile in repo.list().await.unwrap() {
        let typed = profile.to_worker_profile();
        voom_worker_protocol::validate_profile_against_descriptor(&typed)
            .unwrap_or_else(|e| panic!("seed `{}` invalid: {e}", profile.name));
    }
}

#[tokio::test]
async fn get_by_name_returns_profile_or_none() {
    let (repo, _tmp) = repo().await;
    let hit = repo.get_by_name("hevc-archive").await.unwrap().unwrap();
    assert_eq!(hit.codec_profile.as_deref(), Some("main10"));
    assert_eq!(hit.pixel_format.as_deref(), Some("yuv420p10le"));
    assert!(repo.get_by_name("does-not-exist").await.unwrap().is_none());
}

#[tokio::test]
async fn strict_check_rejects_bad_target_codec() {
    let (repo, _tmp) = repo().await;
    let err = sqlx::query(
        "INSERT INTO video_profiles (id, name, target_codec, encoder, crf, preset) \
         VALUES ('x', 'x', 'vp9', 'libx265', 23, 'medium')",
    )
    .execute(repo.pool_for_test())
    .await;
    assert!(err.is_err());
}
```

> Note: the panic in the descriptor-validity test is acceptable in `#[cfg(test)]` code; the workspace `panic` deny applies to non-test code. If clippy still flags it, prefer `assert!(result.is_ok(), ...)`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-store video_profiles`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement** — create `video_profiles.rs`:

```rust
use async_trait::async_trait;
use sqlx::{Row, SqlitePool};
use voom_core::VoomError;
use voom_worker_protocol::TranscodeVideoProfile;

use crate::repo::Repository;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoProfile {
    pub id: String,
    pub name: String,
    pub target_codec: String,
    pub encoder: String,
    pub crf: u8,
    pub preset: String,
    pub tune: Option<String>,
    pub codec_profile: Option<String>,
    pub codec_level: Option<String>,
    pub pixel_format: Option<String>,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub output_container: String,
    pub copy_compatible: bool,
}

impl VideoProfile {
    /// Projects the durable row into the worker-protocol profile, preserving the
    /// registry `name` as the resolved identity.
    #[must_use]
    pub fn to_worker_profile(&self) -> TranscodeVideoProfile {
        TranscodeVideoProfile {
            name: self.name.clone(),
            target_codec: self.target_codec.clone(),
            encoder: self.encoder.clone(),
            crf: self.crf,
            preset: self.preset.clone(),
            tune: self.tune.clone(),
            codec_profile: self.codec_profile.clone(),
            codec_level: self.codec_level.clone(),
            pixel_format: self.pixel_format.clone(),
            max_width: self.max_width,
            max_height: self.max_height,
            copy_compatible: self.copy_compatible,
        }
    }
}

#[async_trait]
pub trait VideoProfileRepo: Repository {
    async fn list(&self) -> Result<Vec<VideoProfile>, VoomError>;
    async fn get_by_name(&self, name: &str) -> Result<Option<VideoProfile>, VoomError>;
}

#[derive(Debug, Clone)]
pub struct SqliteVideoProfileRepo {
    pool: SqlitePool,
}

impl SqliteVideoProfileRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    #[cfg(test)]
    #[must_use]
    pub fn pool_for_test(&self) -> &SqlitePool {
        &self.pool
    }
}

impl Repository for SqliteVideoProfileRepo {}

const SELECT_COLUMNS: &str = "id, name, target_codec, encoder, crf, preset, tune, \
    codec_profile, codec_level, pixel_format, max_width, max_height, output_container, \
    copy_compatible";

#[async_trait]
impl VideoProfileRepo for SqliteVideoProfileRepo {
    async fn list(&self) -> Result<Vec<VideoProfile>, VoomError> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM video_profiles ORDER BY name ASC");
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("video_profiles list: {e}")))?;
        rows.iter().map(row_to_video_profile).collect()
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<VideoProfile>, VoomError> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM video_profiles WHERE name = ?");
        let row = sqlx::query(&sql)
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::Database(format!("video_profiles get_by_name: {e}")))?;
        row.as_ref().map(row_to_video_profile).transpose()
    }
}

fn row_to_video_profile(row: &sqlx::sqlite::SqliteRow) -> Result<VideoProfile, VoomError> {
    let map = |field: &str| move |e: sqlx::Error| VoomError::Database(format!("video_profiles.{field}: {e}"));
    let crf: i64 = row.try_get("crf").map_err(map("crf"))?;
    let copy_compatible: i64 = row.try_get("copy_compatible").map_err(map("copy_compatible"))?;
    let max_width: Option<i64> = row.try_get("max_width").map_err(map("max_width"))?;
    let max_height: Option<i64> = row.try_get("max_height").map_err(map("max_height"))?;
    let to_u32 = |value: i64| u32::try_from(value).map_err(|_| VoomError::Database("video_profiles dimension overflow".to_owned()));
    Ok(VideoProfile {
        id: row.try_get("id").map_err(map("id"))?,
        name: row.try_get("name").map_err(map("name"))?,
        target_codec: row.try_get("target_codec").map_err(map("target_codec"))?,
        encoder: row.try_get("encoder").map_err(map("encoder"))?,
        crf: u8::try_from(crf).map_err(|_| VoomError::Database("video_profiles.crf overflow".to_owned()))?,
        preset: row.try_get("preset").map_err(map("preset"))?,
        tune: row.try_get("tune").map_err(map("tune"))?,
        codec_profile: row.try_get("codec_profile").map_err(map("codec_profile"))?,
        codec_level: row.try_get("codec_level").map_err(map("codec_level"))?,
        pixel_format: row.try_get("pixel_format").map_err(map("pixel_format"))?,
        max_width: max_width.map(to_u32).transpose()?,
        max_height: max_height.map(to_u32).transpose()?,
        output_container: row.try_get("output_container").map_err(map("output_container"))?,
        copy_compatible: copy_compatible != 0,
    })
}

#[cfg(test)]
#[path = "video_profiles_test.rs"]
mod tests;
```

- [ ] **Step 4: Wire the module** — in `crates/voom-store/src/repo/mod.rs`:

```rust
pub mod video_profiles;
pub use video_profiles::{SqliteVideoProfileRepo, VideoProfile, VideoProfileRepo};
```

Add `voom-worker-protocol = { workspace = true }` to `crates/voom-store/Cargo.toml` `[dependencies]` if not already present.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p voom-store video_profiles`
Expected: PASS (all 4 tests).

- [ ] **Step 6: Phase gate + commit**

```bash
just ci
git add crates/voom-store/src/repo/video_profiles.rs crates/voom-store/src/repo/video_profiles_test.rs crates/voom-store/src/repo/mod.rs crates/voom-store/Cargo.toml
git commit -m "feat(store): add read-only video_profiles repository"
```

---

# Phase 3 — Policy: `VideoProfileRef`, Grammar, Compiler Validation

Depends on Phase 1 (capability descriptors). Generalizes `transcode video to hevc` to accept `av1`, `using profile "<name>"`, and an inline `{...}` settings body. Emits `CompiledOperation::TranscodeVideo.profile: VideoProfileRef`.

**The critical contract (spec §Compiled-Policy Compatibility):** `VideoProfileRef` must deserialize a legacy bare JSON string `"default-hevc"` as `Named("default-hevc")`, while new policies serialize the tagged form. Add `voom-worker-protocol = { workspace = true }` to `crates/voom-policy/Cargo.toml` if absent (needed for descriptors).

### Task 3.1: `VideoProfileRef` + `VideoProfileSettings` types with legacy deser

**Files:**
- Create: `crates/voom-policy/src/video_profile.rs`
- Create: `crates/voom-policy/src/video_profile_test.rs`
- Modify: `crates/voom-policy/src/lib.rs`

- [ ] **Step 1: Write the failing test** — create `video_profile_test.rs`:

```rust
use super::*;

#[test]
fn deserializes_legacy_bare_string_as_named() {
    let r: VideoProfileRef = serde_json::from_str("\"default-hevc\"").unwrap();
    assert_eq!(r, VideoProfileRef::Named("default-hevc".to_owned()));
}

#[test]
fn deserializes_tagged_named() {
    let r: VideoProfileRef = serde_json::from_str(r#"{"named":"hevc-archive"}"#).unwrap();
    assert_eq!(r, VideoProfileRef::Named("hevc-archive".to_owned()));
}

#[test]
fn deserializes_tagged_inline() {
    let json = r#"{"inline":{"encoder":"libsvtav1","crf":28,"preset":"6"}}"#;
    let r: VideoProfileRef = serde_json::from_str(json).unwrap();
    match r {
        VideoProfileRef::Inline(s) => {
            assert_eq!(s.encoder, "libsvtav1");
            assert_eq!(s.crf, 28);
            assert_eq!(s.preset, "6");
            assert!(s.tune.is_none());
        }
        VideoProfileRef::Named(_) => panic!("expected inline"),
    }
}

#[test]
fn new_named_serializes_tagged_and_round_trips() {
    let r = VideoProfileRef::Named("default-av1".to_owned());
    let json = serde_json::to_string(&r).unwrap();
    assert_eq!(json, r#"{"named":"default-av1"}"#);
    let back: VideoProfileRef = serde_json::from_str(&json).unwrap();
    assert_eq!(r, back);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-policy video_profile`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement** — create `video_profile.rs`. Serialize as an internally-tagged enum, but use a custom `Deserialize` that also accepts a bare string:

```rust
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

/// Typed inline encode settings. `encoder`, `crf`, `preset` are mandatory in an
/// inline body; the remaining fields are optional. Validation against the
/// per-encoder capability descriptors happens in the compiler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoProfileSettings {
    pub encoder: String,
    pub crf: u8,
    pub preset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tune: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_container: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_compatible: Option<bool>,
}

/// A policy reference to a video encode profile: a registry name or inline
/// settings. Serializes tagged (`{"named": ...}` / `{"inline": ...}`); also
/// deserializes a legacy bare JSON string as `Named` for backward compatibility
/// with compiled policies persisted before Sprint 15.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoProfileRef {
    Named(String),
    Inline(VideoProfileSettings),
}

impl<'de> Deserialize<'de> for VideoProfileRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RefVisitor;

        impl<'de> Visitor<'de> for RefVisitor {
            type Value = VideoProfileRef;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a profile name string or a tagged {named|inline} object")
            }

            // Legacy bare-string form: "default-hevc" -> Named.
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(VideoProfileRef::Named(value.to_owned()))
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let key: String = map
                    .next_key()?
                    .ok_or_else(|| de::Error::custom("empty profile ref object"))?;
                match key.as_str() {
                    "named" => Ok(VideoProfileRef::Named(map.next_value()?)),
                    "inline" => Ok(VideoProfileRef::Inline(map.next_value()?)),
                    other => Err(de::Error::unknown_variant(other, &["named", "inline"])),
                }
            }
        }

        deserializer.deserialize_any(RefVisitor)
    }
}

#[cfg(test)]
#[path = "video_profile_test.rs"]
mod tests;
```

- [ ] **Step 4: Wire the module** — in `crates/voom-policy/src/lib.rs` add `pub mod video_profile;` and re-export:

```rust
pub use video_profile::{VideoProfileRef, VideoProfileSettings};
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p voom-policy video_profile`
Expected: PASS. (Note: `deserialize_any` works here because the input is self-describing JSON; this is the serde approach the spec sanctions, so no `schema_version` bump is needed.)

- [ ] **Step 6: Commit**

```bash
git add crates/voom-policy/src/video_profile.rs crates/voom-policy/src/video_profile_test.rs crates/voom-policy/src/lib.rs crates/voom-policy/Cargo.toml
git commit -m "feat(policy): add VideoProfileRef with legacy bare-string deserialization"
```

### Task 3.2: Parse the inline `{ key: value }` transcode body as settings

Today a transcode statement with a `{}` body parses as `StatementAst::Block` whose `statements` are nested operations (empty for `transcode video to hevc {}`). Sprint 15 needs the transcode block body parsed as `Vec<SettingAst>` (the metadata-block form). Add a dedicated AST carrier so the compiler can read settings without conflating them with nested statements.

**Files:**
- Modify: `crates/voom-policy/src/ast.rs`
- Modify: `crates/voom-policy/src/parser.rs`
- Modify: `crates/voom-policy/src/parser_test.rs`

- [ ] **Step 1: Write the failing test** — add to `parser_test.rs`:

```rust
#[test]
fn parses_transcode_inline_settings_body() {
    let src = "policy \"p\" { phase a { transcode video to av1 { encoder: libsvtav1 crf: 28 preset: 6 } } }";
    let ast = parse_policy(src).unwrap();
    let op = &ast.phases[0].operations[0];
    let StatementAst::TranscodeInline { settings, .. } = op else {
        panic!("expected TranscodeInline, got {op:?}");
    };
    let keys: Vec<&str> = settings.iter().map(|s| s.key.value.as_str()).collect();
    assert_eq!(keys, vec!["encoder", "crf", "preset"]);
}

#[test]
fn parses_bare_transcode_as_raw() {
    let src = "policy \"p\" { phase a { transcode video to hevc } }";
    let ast = parse_policy(src).unwrap();
    assert!(matches!(ast.phases[0].operations[0], StatementAst::Raw { .. }));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-policy parses_transcode`
Expected: FAIL — `TranscodeInline` variant missing.

- [ ] **Step 3: Add the AST variant** — in `ast.rs`, extend `StatementAst`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum StatementAst {
    Raw {
        keyword: Spanned<String>,
        text: String,
        span: crate::SourceSpan,
    },
    Block {
        keyword: Spanned<String>,
        name: Option<Spanned<String>>,
        statements: Vec<StatementAst>,
        span: crate::SourceSpan,
    },
    /// A `transcode ... { key: value ... }` statement whose brace body is a list
    /// of typed settings (reusing the metadata `SettingAst` form), not nested
    /// statements.
    TranscodeInline {
        keyword: Spanned<String>,
        /// The header text before the `{`, e.g. `transcode video to av1`.
        header: String,
        settings: Vec<SettingAst>,
        span: crate::SourceSpan,
    },
}
```

Adding a `StatementAst` variant breaks **every exhaustive match** on it across the `voom-policy` crate — update all of them in this step or the crate will not compile. There are **four** known sites:
- `ast.rs` `StatementAst::span()` and `::keyword()` — add `TranscodeInline { span, .. }` / `TranscodeInline { keyword, .. }` arms.
- `compiled.rs` `statement_text()` (a `Raw`/`Block`-only match around `compiled.rs:831-843`) — add `StatementAst::TranscodeInline { header, .. } => Cow::Borrowed(header.as_str())` (the inline statement's text *is* its header).
- `validate.rs` `statement_text()` — there is a **second, independent copy** of `statement_text` at `validate.rs:922-933` (called unconditionally at `validate.rs:277` for every operation). Add the same `TranscodeInline { header, .. } => Cow::Borrowed(header.as_str())` arm here too. **This is easy to miss because it duplicates the compiled.rs function.**
- Then `rg -n "match .*statement|StatementAst::" crates/voom-policy/src` to confirm no other non-`..` match remains.

Add `crates/voom-policy/src/validate.rs` to this task's `git add` list.

- [ ] **Step 4: Update the parser** — in `parser.rs` `parse_statement`, when the keyword is `transcode` and a `{` follows, parse the body with the existing setting-parsing routine (the same logic `parse_metadata_block` uses) and emit `TranscodeInline` instead of `Block`. Capture the header text (the trimmed text between the keyword start and the `{`). Concretely, factor the metadata key/value loop into a reusable `fn parse_setting_list(&mut self) -> Result<Vec<SettingAst>, ParseError>` (the body of `parse_metadata_block` minus the leading `{`), then:

```rust
// inside parse_statement, after reading `keyword` and locating an opening brace:
if keyword.value == "transcode" {
    let header = self.source[start..open_brace_offset].trim().to_owned();
    self.expect_byte(b'{')?;
    let settings = self.parse_setting_list()?; // consumes through the closing `}`
    let span = self.span_from(start);
    return Ok(StatementAst::TranscodeInline { keyword, header, settings, span });
}
```

Bare `transcode video to hevc` (no brace) keeps the existing `StatementAst::Raw` path unchanged.

> Implementation note: `parse_metadata_block` currently expects `{` then loops keys until `}`. Refactor so `parse_setting_list` assumes the `{` is already consumed and loops to `}`; have `parse_metadata_block` call `expect_byte(b'{')` then `parse_setting_list`. This keeps one key/value parser and is the surgical change the spec's "parser needs no new statement syntax" intends.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p voom-policy parses_transcode`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
# compiled.rs + validate.rs are included because adding the StatementAst variant
# forces their statement_text() exhaustive-match arms to be added now (else the
# crate will not compile). The full lower/validate logic lands in Tasks 3.3/3.4.
git add crates/voom-policy/src/ast.rs crates/voom-policy/src/parser.rs crates/voom-policy/src/parser_test.rs crates/voom-policy/src/compiled.rs crates/voom-policy/src/validate.rs
git commit -m "feat(policy): parse transcode inline settings body as setting list"
```

### Task 3.3: Generalize transcode validation against descriptors

`validate_transcode_statement` currently accepts only `["transcode","video","to","hevc"]`. Generalize to: accept `hevc`/`av1`; accept `using profile "<name>"` (named, passthrough); validate `TranscodeInline` settings against the per-encoder descriptors; reject `using profile` + inline body together; reject unknown/duplicate inline keys, missing mandatory keys, codec/encoder mismatch, out-of-range/unknown values, and incompatible combinations.

**Files:**
- Modify: `crates/voom-policy/src/validate.rs`
- Modify: `crates/voom-policy/src/diagnostic.rs` (reuse `UnsupportedTranscodeShape`; add `InvalidTranscodeProfile` if a distinct code is wanted — keep `UnsupportedTranscodeShape` for shape and add `InvalidVideoProfileSetting` for inline-setting errors)
- Modify: `crates/voom-policy/src/validate_test.rs`

- [ ] **Step 1: Write the failing tests** — replace the Sprint 12 rejection test and add acceptance + new rejection cases in `validate_test.rs`:

```rust
fn codes(src: &str) -> Vec<String> { /* existing helper */ }

#[test]
fn accepts_hevc_and_av1_named_and_inline() {
    assert!(codes("policy \"p\" { phase a { transcode video to hevc } }").is_empty());
    assert!(codes("policy \"p\" { phase a { transcode video to av1 } }").is_empty());
    assert!(codes("policy \"p\" { phase a { transcode video to hevc using profile \"hevc-archive\" } }").is_empty());
    assert!(codes("policy \"p\" { phase a { transcode video to av1 { encoder: libsvtav1 crf: 28 preset: 6 } } }").is_empty());
}

#[test]
fn rejects_invalid_inline_profiles() {
    // missing mandatory encoder
    assert!(codes("policy \"p\" { phase a { transcode video to av1 { crf: 28 preset: 6 } } }")
        .contains(&"invalid_video_profile_setting".to_owned()));
    // encoder/codec mismatch
    assert!(codes("policy \"p\" { phase a { transcode video to av1 { encoder: libx265 crf: 20 preset: medium } } }")
        .contains(&"invalid_video_profile_setting".to_owned()));
    // crf out of range
    assert!(codes("policy \"p\" { phase a { transcode video to hevc { encoder: libx265 crf: 60 preset: medium } } }")
        .contains(&"invalid_video_profile_setting".to_owned()));
    // preset wrong domain (named for svt)
    assert!(codes("policy \"p\" { phase a { transcode video to av1 { encoder: libsvtav1 crf: 30 preset: medium } } }")
        .contains(&"invalid_video_profile_setting".to_owned()));
    // unknown key
    assert!(codes("policy \"p\" { phase a { transcode video to av1 { encoder: libsvtav1 crf: 30 preset: 6 bogus: 1 } } }")
        .contains(&"invalid_video_profile_setting".to_owned()));
    // 10-bit pixel format under 8-bit-only profile
    assert!(codes("policy \"p\" { phase a { transcode video to hevc { encoder: libx265 crf: 20 preset: slow codec_profile: main pixel_format: yuv420p10le } } }")
        .contains(&"invalid_video_profile_setting".to_owned()));
}

#[test]
fn rejects_using_profile_with_inline_body() {
    assert!(codes("policy \"p\" { phase a { transcode video to hevc using profile \"x\" { crf: 20 } } }")
        .contains(&"unsupported_transcode_shape".to_owned()));
}

#[test]
fn rejects_unknown_codec() {
    assert!(codes("policy \"p\" { phase a { transcode video to vp9 } }")
        .contains(&"unsupported_transcode_shape".to_owned()));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-policy transcode`
Expected: FAIL — new diagnostic code and acceptance behavior absent; old test that expected `av1` rejection now removed.

- [ ] **Step 3: Add the diagnostic code** — in `diagnostic.rs`, add `InvalidVideoProfileSetting` to `DiagnosticCode` (its `code` string serializes to `"invalid_video_profile_setting"`; follow the existing snake_case mapping).

- [ ] **Step 4: Implement validation** — rewrite `validate_transcode_statement` in `validate.rs`. Branch on statement form:

```rust
fn validate_transcode_statement(&mut self, statement: &StatementAst) {
    match statement {
        StatementAst::Raw { text, .. } => self.validate_transcode_header(statement, text),
        StatementAst::TranscodeInline { header, settings, .. } => {
            // Inline body present: `using profile` in the header is mutually exclusive.
            if header.contains("using profile") {
                self.error(
                    DiagnosticCode::UnsupportedTranscodeShape,
                    statement.span(),
                    "`using profile` and an inline body are mutually exclusive",
                );
                return;
            }
            let Some(codec) = self.transcode_target_codec(statement, header) else { return };
            self.validate_inline_video_profile(statement, codec, settings);
        }
        StatementAst::Block { .. } => self.error(
            DiagnosticCode::UnsupportedTranscodeShape,
            statement.span(),
            "transcode does not take a nested statement block",
        ),
    }
}
```

`validate_transcode_header` recognizes:
- `transcode video to <hevc|av1>` → ok (Named default).
- `transcode video to <hevc|av1> using profile "<name>"` → ok (Named).
- `transcode audio to <aac|opus> ...` → existing audio path (preserve current behavior).
- anything else → `UnsupportedTranscodeShape`.

`transcode_target_codec` extracts and validates the `<codec>` token (`hevc`/`av1`), emitting `UnsupportedTranscodeShape` for unknown codecs.

`validate_inline_video_profile` enforces, emitting `InvalidVideoProfileSetting` on each violation:
1. duplicate keys (scan `settings`, error on repeat);
2. unknown keys (allowed set: `encoder, crf, preset, tune, codec_profile, codec_level, pixel_format, max_width, max_height, output_container, copy_compatible`);
3. mandatory `encoder`, `crf`, `preset` present;
4. `encoder` resolvable via `voom_worker_protocol::encoder_descriptor`, else error;
5. `descriptor.target_codec == codec` (header codec), else mismatch error;
6. `crf` parses to `u8` and `descriptor.accepts_crf`;
7. `preset` `descriptor.accepts_preset`;
8. optional `tune`/`codec_profile`/`codec_level`/`pixel_format` accepted by descriptor;
9. `descriptor.pixel_format_compatible_with_profile(pixel_format, codec_profile)`;
10. `output_container` ∈ {`mkv`,`mp4`}; `copy_compatible` parses to bool; `max_width`/`max_height` parse to `u32 > 0`.

Read setting values via the `SettingAst.value: ExprAst` arms (`String`/`Identifier`/`Number`/`Boolean`). Encoder/preset/pixel_format come as `Identifier` or `String`; `crf`/`max_*` as `Number`; `copy_compatible` as `Boolean`.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p voom-policy transcode`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-policy/src/validate.rs crates/voom-policy/src/diagnostic.rs crates/voom-policy/src/validate_test.rs
git commit -m "feat(policy): validate hevc/av1 named and inline transcode profiles"
```

### Task 3.4: Lower transcode to `CompiledOperation::TranscodeVideo { profile: VideoProfileRef }`

**Files:**
- Modify: `crates/voom-policy/src/compiled.rs`
- Modify: `crates/voom-policy/src/compiled_test.rs`
- Modify: `crates/voom-policy/src/lib.rs` (no new export; `VideoProfileRef` already exported)

- [ ] **Step 1: Write the failing test** — in `compiled_test.rs`:

```rust
#[test]
fn lowers_bare_hevc_to_named_default() {
    let op = compile_single_op("transcode video to hevc");
    let CompiledOperation::TranscodeVideo { target_codec, container, profile } = op else {
        panic!("expected TranscodeVideo");
    };
    assert_eq!(target_codec, "hevc");
    assert_eq!(container, "mkv");
    assert_eq!(profile, VideoProfileRef::Named("default-hevc".to_owned()));
}

#[test]
fn lowers_bare_av1_to_named_default() {
    let op = compile_single_op("transcode video to av1");
    let CompiledOperation::TranscodeVideo { profile, target_codec, .. } = op else {
        panic!("expected TranscodeVideo");
    };
    assert_eq!(target_codec, "av1");
    assert_eq!(profile, VideoProfileRef::Named("default-av1".to_owned()));
}

#[test]
fn lowers_using_profile_to_named() {
    let op = compile_single_op("transcode video to hevc using profile \"hevc-archive\"");
    let CompiledOperation::TranscodeVideo { profile, .. } = op else { panic!() };
    assert_eq!(profile, VideoProfileRef::Named("hevc-archive".to_owned()));
}

#[test]
fn lowers_inline_to_inline_settings() {
    let op = compile_single_op("transcode video to av1 { encoder: libsvtav1 crf: 28 preset: 6 output_container: mp4 }");
    let CompiledOperation::TranscodeVideo { profile, .. } = op else { panic!() };
    let VideoProfileRef::Inline(s) = profile else { panic!("expected inline") };
    assert_eq!(s.encoder, "libsvtav1");
    assert_eq!(s.crf, 28);
    assert_eq!(s.output_container.as_deref(), Some("mp4"));
}

#[test]
fn legacy_bare_string_profile_round_trips_through_compiled_json() {
    // Simulate a Sprint 12-14 stored compiled doc with a bare-string profile.
    // CompiledOperation is INTERNALLY tagged (`#[serde(tag = "type")]` at
    // compiled.rs:68) -> the variant is `{"type":"transcode_video", ...fields}`,
    // NOT `{"transcode_video": {...}}`. The legacy `profile` is a bare string.
    let op: CompiledOperation = serde_json::from_value(serde_json::json!({
        "type": "transcode_video",
        "target_codec": "hevc",
        "container": "mkv",
        "profile": "default-hevc"
    }))
    .unwrap();
    let CompiledOperation::TranscodeVideo { profile, .. } = op else { panic!() };
    assert_eq!(profile, VideoProfileRef::Named("default-hevc".to_owned()));
}
```

Add a `compile_single_op` helper that compiles a one-phase policy and returns the single operation. The legacy round-trip fixture above uses the internally-tagged shape verbatim — confirm `compiled.rs` still declares `#[serde(tag = "type", rename_all = "snake_case")]` on `CompiledOperation` before relying on it.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-policy compiled`
Expected: FAIL — `profile` field is still `String`.

- [ ] **Step 3: Implement** — in `compiled.rs`:

Change the variant field type and add the resolution-only `resolved_profile` field (this is the **pinned Phase 5↔6 contract** — see the Phase 5 preamble):

```rust
TranscodeVideo {
    target_codec: String,
    container: String,
    profile: crate::VideoProfileRef,
    /// Populated in-memory by the control plane's resolution step
    /// (Phase 6) before planning; never written to `compiled_json`
    /// (skipped when `None`, defaults to `None` on read) so stored
    /// rows and `source_hash` are unaffected and legacy bare-string
    /// policies still deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolved_profile: Option<voom_worker_protocol::TranscodeVideoProfile>,
},
```

`voom-policy` already depends on `voom-worker-protocol` (added in Task 3.1 for the descriptors); per spec §4 worker-protocol is "a shared dependency of policy, plan, and worker", so this is layering-clean. When the compiler lowers a transcode statement it sets `resolved_profile: None`; only Phase 6 fills it. Add a regression test asserting a freshly-compiled policy serializes **without** a `resolved_profile` key (so `compiled_json` is unchanged from Sprint 14 shape aside from the `profile` tagging).

**Dispatch `TranscodeInline` explicitly — do not route it through `statement_text`.** `lower_operation` currently dispatches by calling `statement_text(statement)` then matching the first token. A `TranscodeInline` statement has no operation body in its text, so it must be intercepted by matching the enum variant *before* the `statement_text`-based keyword dispatch:

```rust
fn lower_operation(source: &str, statement: &StatementAst)
    -> Result<CompiledOperation, Vec<PolicyDiagnostic>>
{
    if let StatementAst::TranscodeInline { header, settings, .. } = statement {
        return lower_transcode_inline(source, statement, header, settings);
    }
    let text = statement_text(statement);
    let tokens = words(text.as_ref());
    // ... existing keyword match; the `"transcode"` arm becomes lower_transcode_raw ...
}
```

`lower_transcode_raw` parses the `Raw` header tokens:
- `["transcode","video","to", codec]` → `profile = Named(format!("default-{codec}"))`, `target_codec = codec`, `container = "mkv"`, `resolved_profile = None`.
- `["transcode","video","to", codec, "using","profile", "\"name\""]` → `profile = Named(name)` (strip quotes), `target_codec = codec`, `container = "mkv"`, `resolved_profile = None`.
- audio path unchanged.

`lower_transcode_inline` builds `VideoProfileSettings` from the validated `settings` (validation in Task 3.3 already guarantees well-formedness) and emits `profile = Inline(settings)`, `target_codec` from the header, `container = settings.output_container.unwrap_or("mkv")`, `resolved_profile = None`.

> The `container` field is provisional after lowering. The control-plane resolver (Phase 6) overwrites `target_codec`/`container` and fills `resolved_profile` from the registry/inline settings before planning. The planner reads `container` from this operation field (set by Phase 6) and the profile knobs from `resolved_profile` — note `TranscodeVideoProfile` itself has **no** `output_container` field (container rides the worker request's `TranscodeVideoOutput`), so the operation's `container` field is the single source of the output container through the planner and ticket payload.

**Cross-crate compile coupling (do this in Step 3, before the `just ci` gate).** Changing `profile: String → VideoProfileRef` and adding `resolved_profile` breaks every non-`..` destructure / struct-literal of `CompiledOperation::TranscodeVideo` in the workspace. `just ci` compiles all crates, so Phase 3 must fix all of them now (full planner *logic* still lands in Phase 5; here it's a compile-preserving adaptation). Update each site found by `rg -n "CompiledOperation::TranscodeVideo" crates`:
- `crates/voom-policy/src/compiled_test.rs:26` — struct literal: add `profile: VideoProfileRef::Named(...)`, `resolved_profile: None`.
- `crates/voom-policy/src/pipeline_test.rs:11` — struct literal: same.
- `crates/voom-plan/src/planner.rs:273` — named destructure `{ target_codec, container, profile }`: change to also bind `resolved_profile` (or `, ..`). This site currently feeds `profile: &str` into `expand_transcode_video_for_snapshot`; for Phase 3 keep Sprint-12 behavior by deriving the dispatch string from the `VideoProfileRef` (`Named(n) => n`, `Inline(_) => "inline"`), passing the existing `&str` param. Phase 5 Task 5.2 replaces this call to consume `resolved_profile` instead.
- `crates/voom-plan/src/planner_test.rs:231` and `:1530` — struct literals: add `profile: VideoProfileRef::Named("default".into())`, `resolved_profile: None`. (Phase 5 rewrites these helpers to populate `resolved_profile: Some(...)`.)
- `planner.rs:1795` already uses `{ .. }` — no change needed.

Add `crates/voom-plan/src/planner.rs`, `crates/voom-plan/src/planner_test.rs`, and `crates/voom-policy/src/pipeline_test.rs` to the `git add` list for this task's commit.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p voom-policy`
Expected: PASS (all policy tests, including the legacy round-trip).

- [ ] **Step 5: Phase gate + commit**

```bash
just ci
git add crates/voom-policy/src/compiled.rs crates/voom-policy/src/compiled_test.rs crates/voom-policy/src/pipeline_test.rs crates/voom-plan/src/planner.rs crates/voom-plan/src/planner_test.rs
git commit -m "feat(policy): lower transcode to typed VideoProfileRef"
```

---

# Phase 4 — ffprobe Normalizer + Planning-Input Projection

Independent of Phases 1–3 (can be done in parallel). The planner (Phase 5) depends on it. The normalizer must capture video stream `pixel_format`, `profile`, and `level`; the projection must carry video `width`/`height` and surface per-stream `kind`+`codec_name` (already present in `stream_summary.streams`) plus the new fields.

### Task 4.1: Capture `pixel_format`, `profile`, `level` in the normalizer

**Files:**
- Modify: `crates/voom-ffprobe-worker/src/normalize.rs`
- Modify: `crates/voom-ffprobe-worker/src/normalize_test.rs`

- [ ] **Step 1: Write the failing test** — add to `normalize_test.rs` (extend an existing video-stream fixture to include `pix_fmt`, `profile`, `level`):

```rust
#[test]
fn captures_video_pixel_format_profile_and_level() {
    let raw = serde_json::json!({
        "format": {"format_name": "matroska,webm", "duration": "10.0"},
        "streams": [{
            "index": 0, "codec_type": "video", "codec_name": "hevc",
            "width": 1920, "height": 1080,
            "pix_fmt": "yuv420p10le", "profile": "Main 10", "level": 153
        }]
    });
    let snapshot = normalize_ffprobe_json(raw, "ffprobe 7.0", "2026-05-28T00:00:00Z").unwrap();
    let stream = &snapshot["streams"][0];
    assert_eq!(stream["pixel_format"], "yuv420p10le");
    assert_eq!(stream["profile"], "Main 10");
    assert_eq!(stream["level"], "153");
}

#[test]
fn omits_absent_video_profile_fields() {
    let raw = serde_json::json!({
        "streams": [{"index": 0, "codec_type": "video", "codec_name": "hevc", "width": 1, "height": 1}]
    });
    let snapshot = normalize_ffprobe_json(raw, "v", "t").unwrap();
    let stream = snapshot["streams"][0].as_object().unwrap();
    assert!(!stream.contains_key("pixel_format"));
    assert!(!stream.contains_key("profile"));
    assert!(!stream.contains_key("level"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-ffprobe-worker captures_video_pixel_format`
Expected: FAIL — fields not captured.

- [ ] **Step 3: Implement** — in `normalize.rs` `stream_objects`, after the existing inserts add (reusing the existing `insert_string`/`insert_string_as` helpers, which already filter sentinel values like `N/A`/`unknown`):

```rust
insert_string_as(input, &mut output, "pix_fmt", "pixel_format");
insert_string(input, &mut output, "profile");
// ffprobe reports `level` as a number; normalize to string for stable comparison.
insert_u64_as_string(input, &mut output, "level", "level");
```

Add a small helper `insert_u64_as_string` mirroring `insert_u64_value` but stringifying (some ffprobe builds emit `level` as a JSON number, e.g. `153`). If `level` arrives as a string already, fall back to `insert_string`. Keep the sentinel-filtering behavior.

Bump the snapshot format token from `"sprint10-v1"` to `"sprint15-v1"` **only if** the planner/consumers gate on it; the explorer found consumers read fields directly, so the token bump is cosmetic. Leave the token as `"sprint10-v1"` to avoid churn unless a consumer test asserts it — confirm by grepping `rg "sprint10-v1"` and updating any asserting test deliberately.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p voom-ffprobe-worker`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-ffprobe-worker/src/normalize.rs crates/voom-ffprobe-worker/src/normalize_test.rs
git commit -m "feat(ffprobe): capture video pixel format, profile, and level"
```

### Task 4.2: Project video `width`/`height` into the planning input

The projection currently sets `width`/`height` to `None`. The planner needs them for dimension compliance. Derive them from the single video stream in `stream_summary.streams`.

**Files:**
- Modify: `crates/voom-control-plane/src/media_snapshot.rs`
- Modify: `crates/voom-control-plane/src/media_snapshot_test.rs`

- [ ] **Step 1: Write the failing test** — add to `media_snapshot_test.rs`:

```rust
#[test]
fn planning_input_projects_video_dimensions() {
    let snapshot = snapshot_with_payload(serde_json::json!({
        "container": "matroska",
        "streams": [{
            "id": "stream-0", "index": 0, "kind": "video", "codec_name": "h264",
            "width": 3840, "height": 2160, "pixel_format": "yuv420p"
        }]
    }));
    let input = planning_input(&snapshot);
    assert_eq!(input.width, Some(3840));
    assert_eq!(input.height, Some(2160));
}
```

(Reuse or add a `snapshot_with_payload` test helper that wraps a JSON payload in a `MediaSnapshot`.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-control-plane planning_input_projects_video_dimensions`
Expected: FAIL — `width`/`height` are `None`.

- [ ] **Step 3: Implement** — in `media_snapshot.rs` `planning_input`, replace the `width: None, height: None` lines with values derived from the single video stream:

```rust
let video_stream = streams
    .as_array()
    .and_then(|streams| {
        streams
            .iter()
            .find(|s| s.get("kind").and_then(Value::as_str) == Some("video"))
    });
let dimension = |key: &str| {
    video_stream
        .and_then(|s| s.get(key))
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
};
// ...
width: dimension("width"),
height: dimension("height"),
```

`pixel_format`/`profile`/`level` are already carried inside `stream_summary.streams` for the planner to read per-stream; no new top-level field is needed.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p voom-control-plane media_snapshot`
Expected: PASS.

- [ ] **Step 5: Phase gate + commit**

```bash
just ci
git add crates/voom-control-plane/src/media_snapshot.rs crates/voom-control-plane/src/media_snapshot_test.rs
git commit -m "feat(control-plane): project video dimensions into planning input"
```

---

# Phase 5 — Planner: Profile-Aware Compliance + Resource Notes

Depends on Phase 1 (`TranscodeVideoProfile`), Phase 4 (dimensions + per-stream facts). The planner consumes a fully-typed resolved profile (supplied by Phase 6 at planning-input assembly). This phase introduces the resolved-profile view type, the `inline-<hash>` identity, the compliance logic, MP4 gating, and resource notes — all unit-tested against hand-built profiles and snapshots, so it does not need Phase 6 wired yet.

**PINNED Phase 5↔6 contract (read before writing any Phase 5 test).** The planner stays pure (no store, no name resolution). The single resolution point is the control plane (Phase 6), which fills the `resolved_profile: Option<TranscodeVideoProfile>` field added to `CompiledOperation::TranscodeVideo` in Task 3.4 **in memory**, after the policy is deserialized and identity-checked but before `generate_plan` runs. The planner reads `resolved_profile` (a fully-typed `voom_worker_protocol::TranscodeVideoProfile`, which carries the resolved `name` — built-in registry name or `inline-<hash>`) and the operation's `container` field for the output container. The `profile: VideoProfileRef` field is left as-is for audit.

This was deliberately chosen over two rejected alternatives:
- *Rewriting `profile` to `VideoProfileRef::Inline(VideoProfileSettings)`* is **lossy**: `VideoProfileSettings` has no `name`, so a resolved `Named` profile would lose its registry identity, which the spec (§Resolution, §7 target naming) requires for the target-path discriminator and reports. Rejected.
- *A free-form `serde_json::Value` payload key* is weaker-typed than the `Option<TranscodeVideoProfile>` field and gives no compile-time guarantee. Rejected.

**Invariant the planner enforces:** by the time `generate_plan` reaches a `transcode_video` operation, `resolved_profile` is `Some`. If it is `None`, that is an internal error (resolution was skipped) — the planner returns a `PlanGenerationError`, it does not silently no-op. Phase 5 tests construct `CompiledPolicy` values with `resolved_profile: Some(...)` already populated (simulating Phase 6), so Phase 5 is fully testable before Phase 6 is wired. `expand_transcode_video_for_snapshot` changes its `profile: &str` parameter (current `planner.rs:456`) to `resolved: &voom_worker_protocol::TranscodeVideoProfile` plus `container: &str`.

### Task 5.1: Resolved-profile view + `inline-<hash>` identity + cpu-cost lookup

**Files:**
- Create: `crates/voom-plan/src/transcode_video_profile.rs`
- Create: `crates/voom-plan/src/transcode_video_profile_test.rs`
- Modify: `crates/voom-plan/src/lib.rs`

- [ ] **Step 1: Write the failing test** — create `transcode_video_profile_test.rs`:

```rust
use super::*;

#[test]
fn inline_hash_is_stable_across_serde_round_trip() {
    let settings = sample_settings(); // libsvtav1, crf 30, preset 8
    let h1 = inline_profile_id(&settings);
    let json = serde_json::to_string(&settings).unwrap();
    let back: voom_policy::VideoProfileSettings = serde_json::from_str(&json).unwrap();
    let h2 = inline_profile_id(&back);
    assert_eq!(h1, h2);
    assert!(h1.starts_with("inline-"));
    assert_eq!(h1.len(), "inline-".len() + 12);
}

#[test]
fn inline_hash_differs_for_near_identical_profiles() {
    let mut a = sample_settings();
    a.crf = 22;
    let mut b = sample_settings();
    b.crf = 23;
    assert_ne!(inline_profile_id(&a), inline_profile_id(&b));
}

#[test]
fn cpu_cost_lookup_is_deterministic() {
    assert_eq!(cpu_cost("libx265", "placebo"), "high");
    assert_eq!(cpu_cost("libx265", "medium"), "medium");
    assert_eq!(cpu_cost("libaom-av1", "0"), "high");
    assert_eq!(cpu_cost("libsvtav1", "8"), "low");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-plan transcode_video_profile`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement** — create `transcode_video_profile.rs`. The hash is over a **canonical, version-stable** representation (fixed field order, normalized/lowercased tokens, absent optionals omitted) — not raw serde output:

```rust
use voom_policy::VideoProfileSettings;

/// Deterministic `inline-<hash>` identity for an inline profile, computed over a
/// canonical representation so it does not drift with serde layout changes.
#[must_use]
pub fn inline_profile_id(settings: &VideoProfileSettings) -> String {
    let canonical = canonical_form(settings);
    let digest = blake3::hash(canonical.as_bytes());
    format!("inline-{}", &digest.to_hex()[..12])
}

fn canonical_form(s: &VideoProfileSettings) -> String {
    let mut parts = Vec::new();
    parts.push(format!("encoder={}", s.encoder.to_ascii_lowercase()));
    parts.push(format!("crf={}", s.crf));
    parts.push(format!("preset={}", s.preset.trim()));
    if let Some(v) = &s.tune { parts.push(format!("tune={}", v.to_ascii_lowercase())); }
    if let Some(v) = &s.codec_profile { parts.push(format!("codec_profile={}", v.to_ascii_lowercase())); }
    if let Some(v) = &s.codec_level { parts.push(format!("codec_level={v}")); }
    if let Some(v) = &s.pixel_format { parts.push(format!("pixel_format={}", v.to_ascii_lowercase())); }
    if let Some(v) = s.max_width { parts.push(format!("max_width={v}")); }
    if let Some(v) = s.max_height { parts.push(format!("max_height={v}")); }
    if let Some(v) = &s.output_container { parts.push(format!("output_container={}", v.to_ascii_lowercase())); }
    if let Some(v) = s.copy_compatible { parts.push(format!("copy_compatible={v}")); }
    parts.join(";")
}

/// Fixed encoder+speed → cpu cost class lookup for resource notes.
#[must_use]
pub fn cpu_cost(encoder: &str, speed: &str) -> &'static str {
    match (encoder, speed) {
        ("libx265", "placebo" | "veryslow") => "high",
        ("libx265", "slower" | "slow" | "medium") => "medium",
        ("libx265", _) => "low",
        ("libaom-av1", "0" | "1" | "2") => "high",
        ("libaom-av1", "3" | "4") => "medium",
        ("libaom-av1", _) => "low",
        ("libsvtav1", s) => match s.parse::<u8>() {
            Ok(0..=3) => "high",
            Ok(4..=7) => "medium",
            _ => "low",
        },
        _ => "medium",
    }
}

#[cfg(test)]
#[path = "transcode_video_profile_test.rs"]
mod tests;
```

Add `pub mod transcode_video_profile;` to `crates/voom-plan/src/lib.rs` with re-exports `pub use transcode_video_profile::{cpu_cost, inline_profile_id};`. Ensure `voom-policy` and `blake3` are deps (they are).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p voom-plan transcode_video_profile`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-plan/src/transcode_video_profile.rs crates/voom-plan/src/transcode_video_profile_test.rs crates/voom-plan/src/lib.rs
git commit -m "feat(plan): add inline profile identity and cpu-cost lookup"
```

### Task 5.2: Profile-aware compliance + MP4 gating + resource notes

Rewrite `transcode_video_shape` to read the resolved profile from the operation payload and apply spec §5 rules. Build resource notes in `make_node` for transcode_video.

**Files:**
- Modify: `crates/voom-plan/src/planner.rs`
- Modify: `crates/voom-plan/src/planner_test.rs`

- [ ] **Step 1: Write the failing tests** — extend `planner_test.rs`. Build snapshots via a helper that includes a video stream with codec/dims/pixel_format/profile/level and optional non-video streams. Cover every §5 branch:

```rust
#[test]
fn no_op_when_all_observable_constraints_satisfied() {
    // profile: hevc, mkv, max 1920x1080, pixel_format yuv420p
    // source: hevc, matroska, 1280x720, yuv420p  -> NoOp
    let plan = plan_transcode(profile_hevc_1080p_mkv(), source_hevc_720_mkv());
    assert_eq!(node_status(&plan), NodeStatus::NoOp);
}

#[test]
fn planned_when_too_wide() {
    // source 3840x2160 exceeds 1920 cap -> Planned
    let plan = plan_transcode(profile_hevc_1080p_mkv(), source_hevc_2160_mkv());
    assert_eq!(node_status(&plan), NodeStatus::Planned);
}

#[test]
fn planned_on_container_change() {
    // profile container mp4, source matroska, otherwise compliant -> Planned
    let plan = plan_transcode(profile_hevc_mp4(), source_hevc_720_mkv());
    assert_eq!(node_status(&plan), NodeStatus::Planned);
}

#[test]
fn planned_on_wrong_pixel_format_or_profile_level() {
    let plan = plan_transcode(profile_hevc_10bit(), source_hevc_8bit());
    assert_eq!(node_status(&plan), NodeStatus::Planned);
}

#[test]
fn blocked_insufficient_when_constrained_pixel_format_unknown() {
    // profile constrains pixel_format; source stream omits it -> Blocked
    let plan = plan_transcode(profile_hevc_10bit(), source_without_pixel_format());
    assert_eq!(node_status(&plan), NodeStatus::Blocked);
}

#[test]
fn blocked_unsupported_when_not_exactly_one_video_stream() {
    let plan = plan_transcode(profile_hevc_mp4(), source_two_video_streams());
    assert_eq!(node_status(&plan), NodeStatus::Blocked);
}

#[test]
fn blocked_when_mp4_target_has_incompatible_subtitle() {
    // mp4 profile + source with an ASS subtitle stream -> Blocked, names the stream
    let plan = plan_transcode(profile_hevc_mp4(), source_with_ass_subtitle());
    assert_eq!(node_status(&plan), NodeStatus::Blocked);
    assert!(blocked_reason(&plan).contains("ass"));
}

#[test]
fn blocked_insufficient_when_mp4_stream_inventory_underdescribed() {
    // mp4 profile + a non-video stream missing codec_name -> Blocked insufficient
    let plan = plan_transcode(profile_hevc_mp4(), source_stream_missing_codec());
    assert_eq!(node_status(&plan), NodeStatus::Blocked);
}

#[test]
fn resource_notes_are_format_stable() {
    let plan = plan_transcode(profile_hevc_1080p_mkv(), source_hevc_2160_mkv());
    let notes = resource_notes(&plan);
    assert!(notes.contains(&"encoder=libx265".to_owned()));
    assert!(notes.contains(&"speed=medium".to_owned()));
    assert!(notes.contains(&"cpu_cost=medium".to_owned()));
    assert!(notes.contains(&"crf=23".to_owned()));
    assert!(notes.contains(&"downscale=3840x2160->1920x1080".to_owned()));
}
```

Add the profile/source builder helpers and `node_status`/`blocked_reason`/`resource_notes` accessors. The `plan_transcode` helper builds a one-phase compiled policy whose `CompiledOperation::TranscodeVideo` has `resolved_profile: Some(<typed profile>)` and `container` set (simulating Phase 6's in-memory resolution per the pinned contract), then runs `generate_plan`. Add one test asserting that a `transcode_video` operation with `resolved_profile: None` makes `generate_plan` return a `PlanGenerationError` rather than silently no-op'ing.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-plan transcode_video`
Expected: FAIL — current shape logic ignores dimensions/pixel-format/MP4.

- [ ] **Step 3: Implement** — in `planner.rs`:

At the dispatch site that calls `expand_transcode_video_for_snapshot`, read the operation's `resolved_profile` — if `None`, return `PlanGenerationError` (resolution invariant, per the pinned contract); otherwise pass the typed `&TranscodeVideoProfile` and the operation's `&container` into `expand_transcode_video_for_snapshot` (whose signature changes from `profile: &str` to `resolved: &TranscodeVideoProfile, container: &str`).

> **Sequencing (avoid an intra-phase `just ci` red window):** Task 3.4's cross-crate compile-fix left `planner_test.rs:231` and `:1530` with `resolved_profile: None`. Adding the None→`PlanGenerationError` invariant in *this* step makes those two literals fail. **In the same commit**, update both to `resolved_profile: Some(<valid profile>)` (e.g. `TranscodeVideoProfile::default_hevc()` plus the codec/dims the test asserts) so `just ci` stays green. The dedicated None→error test (Step 1) uses its own fresh operation.

Replace `transcode_video_shape` with logic that, in order:

1. Resolve the single video stream from `stream_summary.streams`; if `video_stream_count != 1` → `UnsupportedShape("transcode_video requires exactly one video stream")`.
2. If container unknown → `InsufficientFacts`.
3. If codec unknown → `InsufficientFacts`.
4. Compute `needs_change` accumulator over observable constraints (`resolved` is the typed `&TranscodeVideoProfile`; `target_container` is the operation's `container` arg — `TranscodeVideoProfile` has no `output_container` field):
   - codec != `resolved.target_codec` → needs re-encode;
   - container != `target_container` → needs change (container);
   - dims constrained (`resolved.max_width`/`max_height`) and unknown → `InsufficientFacts`; else exceed cap → needs change;
   - `resolved.pixel_format` constrained and unknown → `InsufficientFacts`; else mismatch → needs change;
   - `resolved.codec_profile`/`codec_level` constrained and unknown → `InsufficientFacts`; else mismatch → needs change.
5. MP4 gate (when `target_container == "mp4"`): iterate non-video streams. If any stream is missing `kind` or `codec_name` → `InsufficientFacts` ("mp4 target requires fully enumerated streams"). If any non-video stream's `(kind, codec_name)` is not in the MP4-muxable allowlist (audio: aac/ac3/eac3/opus; video: hevc/av1) → `UnsupportedShape` naming the offending stream(s).
6. If no change needed → `Compliant`; else `NeedsTranscode`.

Use `eq_ignore_ascii_case` for codec; normalize ffprobe codec profile tokens (e.g. ffprobe `"Main 10"` vs profile `"main10"`) via a small mapping helper, or compare case-insensitively after stripping spaces. Document the normalization in a comment.

Then in `make_node` (or a transcode-specific builder), populate `ResourceEstimates.notes` only for `transcode_video` planned nodes:

```rust
let mut notes = vec![
    format!("encoder={}", profile.encoder),
    format!("speed={}", profile.preset),
    format!("cpu_cost={}", cpu_cost(&profile.encoder, &profile.preset)),
    format!("crf={}", profile.crf),
];
if let (Some(cap_w), Some(cap_h), Some(src_w), Some(src_h)) =
    (resolved.max_width, resolved.max_height, snapshot.width, snapshot.height)
{
    if src_w > cap_w || src_h > cap_h {
        notes.push(format!("downscale={src_w}x{src_h}->{cap_w}x{cap_h}"));
    }
}
```

**PINNED node `operation_payload` schema (the contract `workflow/binding.rs` reads in Phase 6 — Finding from review).** `expand_transcode_video_for_snapshot` must emit exactly these keys so the ticket-payload renderer keeps working and the worker request can be built downstream:

```rust
let payload = json!({
    "type": "transcode_video",
    "target_codec": resolved.target_codec,   // string
    "container": container,                   // string, the operation's container arg
    "profile": resolved.name,                 // string: the RESOLVED identity
                                              //   (registry name or "inline-<hash>")
    "resolved_profile": serde_json::to_value(resolved)?, // full TranscodeVideoProfile JSON
});
```

The `"profile"` key stays a **bare string** (now the resolved name), so the existing `binding.rs::required_string(operation_payload, "profile")` continues to return a valid value (it surfaces the resolved name in the ticket payload/reports). The new `"resolved_profile"` object carries the full typed profile for `binding.rs` to thread into the worker request (Phase 6 Task 6.4). Add a planner test asserting the node payload contains both `"profile"` (string == resolved name) and a `"resolved_profile"` object whose `encoder`/`crf` match.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p voom-plan`
Expected: PASS.

- [ ] **Step 5: Phase gate + commit**

```bash
just ci
git add crates/voom-plan/src/planner.rs crates/voom-plan/src/planner_test.rs
git commit -m "feat(plan): profile-aware transcode compliance, mp4 gating, resource notes"
```

---

# Phase 6 — Control Plane: Resolution, Dispatch, Execution, Target Naming

Depends on Phases 1, 2, 3, 5. This is the integration spine: it wires the `video_profiles` repo into `ControlPlane`, resolves `Named` refs (and assigns `inline-<hash>` identities) at planning-input assembly, computes the `copy_video` decision, carries the resolved profile through dispatch, and names target paths with the profile-identity discriminator.

### Task 6.1: Wire `SqliteVideoProfileRepo` into `ControlPlane`

**Files:**
- Modify: `crates/voom-control-plane/src/lib.rs`
- Modify: `crates/voom-control-plane/Cargo.toml` (ensure `voom-store` re-exports the repo; no new dep)

- [ ] **Step 1: Add the field** — in `ControlPlane` struct add `pub(crate) video_profiles: SqliteVideoProfileRepo,` and construct it in `ControlPlane::open` alongside the other repos: `video_profiles: SqliteVideoProfileRepo::new(pool.clone()),`.

- [ ] **Step 2: Write a smoke test** — add a control-plane unit test asserting `cp.video_profiles.list()` returns the 6 seeded built-ins after `open` on a fresh DB. (Sibling `lib_test.rs` or the nearest existing control-plane test module.)

- [ ] **Step 3: Run / verify**

Run: `cargo test -p voom-control-plane video_profiles`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-control-plane/src/lib.rs
git commit -m "feat(control-plane): expose video_profiles repository"
```

### Task 6.2: Resolution step — `VideoProfileRef` → typed profile (+ inline identity)

Resolution happens at planning-input assembly (`generate_compliance_report` / the plan-only `plan_compiled_policy_with_input` path) so both the executing and dry-run paths share it (spec §Resolution: "no execute-only resolution path"). It rewrites each `CompiledOperation::TranscodeVideo` in the compiled policy to carry a fully-resolved profile before `generate_plan`.

**Files:**
- Create: `crates/voom-control-plane/src/transcode/resolve.rs`
- Create: `crates/voom-control-plane/src/transcode/resolve_test.rs`
- Modify: `crates/voom-control-plane/src/transcode/mod.rs` (`pub mod resolve;`)

- [ ] **Step 1: Write the failing test** — `resolve_test.rs`:

```rust
use super::*;

#[tokio::test]
async fn resolves_named_profile_to_typed_settings() {
    let (repo, _tmp) = seeded_repo().await;
    let resolved = resolve_video_profile_ref(
        &repo,
        &voom_policy::VideoProfileRef::Named("hevc-archive".to_owned()),
    ).await.unwrap();
    assert_eq!(resolved.profile.name, "hevc-archive");
    assert_eq!(resolved.profile.pixel_format.as_deref(), Some("yuv420p10le"));
    assert_eq!(resolved.output_container, "mkv");
}

#[tokio::test]
async fn unknown_named_profile_is_config_invalid() {
    let (repo, _tmp) = seeded_repo().await;
    let err = resolve_video_profile_ref(
        &repo,
        &voom_policy::VideoProfileRef::Named("nope".to_owned()),
    ).await.unwrap_err();
    assert_eq!(err.code(), "CONFIG_INVALID");
}

#[tokio::test]
async fn inline_profile_gets_synthetic_identity() {
    let (repo, _tmp) = seeded_repo().await;
    let settings = inline_av1_settings(); // libsvtav1, crf 28, preset 6, mp4
    let resolved = resolve_video_profile_ref(
        &repo,
        &voom_policy::VideoProfileRef::Inline(settings),
    ).await.unwrap();
    assert!(resolved.profile.name.starts_with("inline-"));
    assert_eq!(resolved.output_container, "mp4");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-control-plane resolve_video_profile`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement** — `resolve.rs`:

```rust
use voom_core::VoomError;
use voom_plan::inline_profile_id;
use voom_policy::{VideoProfileRef, VideoProfileSettings};
use voom_store::{VideoProfileRepo, SqliteVideoProfileRepo};
use voom_worker_protocol::TranscodeVideoProfile;

pub struct ResolvedProfile {
    pub profile: TranscodeVideoProfile,
    pub output_container: String,
}

/// Resolves a policy profile reference into a fully-typed worker profile.
/// `Named` references are looked up in the registry (unknown -> `CONFIG_INVALID`);
/// `Inline` settings are assigned a deterministic `inline-<hash>` identity.
///
/// # Errors
/// Returns `CONFIG_INVALID` when a named profile does not exist or inline
/// settings fail descriptor validation.
pub async fn resolve_video_profile_ref(
    repo: &SqliteVideoProfileRepo,
    reference: &VideoProfileRef,
) -> Result<ResolvedProfile, VoomError> {
    match reference {
        VideoProfileRef::Named(name) => {
            let row = repo.get_by_name(name).await?.ok_or_else(|| {
                VoomError::Config(format!("unknown video profile `{name}`"))
            })?;
            Ok(ResolvedProfile {
                output_container: row.output_container.clone(),
                profile: row.to_worker_profile(),
            })
        }
        VideoProfileRef::Inline(settings) => {
            let profile = inline_to_worker_profile(settings)?;
            // Belt-and-braces: validate even though the compiler already did.
            voom_worker_protocol::validate_profile_against_descriptor(&profile)
                .map_err(VoomError::Config)?;
            Ok(ResolvedProfile {
                output_container: settings
                    .output_container
                    .clone()
                    .unwrap_or_else(|| "mkv".to_owned()),
                profile,
            })
        }
    }
}

fn inline_to_worker_profile(s: &VideoProfileSettings) -> Result<TranscodeVideoProfile, VoomError> {
    let descriptor = voom_worker_protocol::encoder_descriptor(&s.encoder)
        .ok_or_else(|| VoomError::Config(format!("unknown encoder `{}`", s.encoder)))?;
    Ok(TranscodeVideoProfile {
        name: inline_profile_id(s),
        target_codec: descriptor.target_codec.to_owned(),
        encoder: s.encoder.clone(),
        crf: s.crf,
        preset: s.preset.clone(),
        tune: s.tune.clone(),
        codec_profile: s.codec_profile.clone(),
        codec_level: s.codec_level.clone(),
        pixel_format: s.pixel_format.clone(),
        max_width: s.max_width,
        max_height: s.max_height,
        copy_compatible: s.copy_compatible.unwrap_or(false),
    })
}

#[cfg(test)]
#[path = "resolve_test.rs"]
mod tests;
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p voom-control-plane resolve_video_profile`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-control-plane/src/transcode/resolve.rs crates/voom-control-plane/src/transcode/resolve_test.rs crates/voom-control-plane/src/transcode/mod.rs
git commit -m "feat(control-plane): resolve video profile refs to typed profiles"
```

### Task 6.3: Resolve before planning (store-backed paths + offline inline-only)

The planner (Phase 5) consumes resolved profiles. Resolution populates each transcode operation's `resolved_profile` field (and overwrites `target_codec`/`container`) in memory before the pure planner runs. Resolution is store-backed (the registry is in SQLite), so it is wired into the two `&ControlPlane` plan entry points; the store-free offline `voom plan <file>` path resolves inline profiles only.

**Files:**
- Modify: `crates/voom-control-plane/src/cases/compliance.rs` (execute path: call `resolve_profiles_in_policy`)
- Modify: `crates/voom-control-plane/src/cases/plans.rs` (add `resolve_profiles_in_policy`; call it in `plan_accepted_policy_version_with_input_set`; call the sync `resolve_inline_profiles_in_policy` in `plan_policy_source_with_input`)
- Modify: `crates/voom-control-plane/src/transcode/resolve.rs` (add the sync `resolve_inline_profiles_in_policy` from Step 3)
- Modify: existing compliance/plan test files; add a dry-run-parity test on `plan_accepted_policy_version_with_input_set`

- [ ] **Step 1: Write the failing test** — add a control-plane test (in `compliance.rs`'s sibling test or `tests/`) that:
  - a policy with `transcode video to hevc using profile "nonexistent"` produces a `CONFIG_INVALID` diagnostic and **no plan**;
  - a policy with `transcode video to hevc` plans the resolved `default-hevc` profile (assert the planned node payload carries `encoder=libx265`, `crf=23`).

```rust
#[tokio::test]
async fn unknown_named_profile_blocks_before_planning() {
    let cp = seeded_control_plane().await;
    let err = generate_compliance_report_for_policy(&cp, "transcode video to hevc using profile \"nope\"").await;
    assert!(err.is_err());
    assert_eq!(err.unwrap_err().code(), "CONFIG_INVALID");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-control-plane unknown_named_profile_blocks`
Expected: FAIL — resolution not wired; unknown name slips through.

- [ ] **Step 3: Implement** — follow the **pinned Phase 5↔6 contract** (Phase 5 preamble). Add `cases/plans.rs::resolve_profiles_in_policy(cp: &ControlPlane, policy: &mut CompiledPolicy) -> Result<(), VoomError>`:

```rust
pub(crate) async fn resolve_profiles_in_policy(
    cp: &ControlPlane,
    policy: &mut voom_policy::CompiledPolicy,
) -> Result<(), VoomError> {
    for phase in &mut policy.phases {
        for operation in &mut phase.operations {
            if let voom_policy::CompiledOperation::TranscodeVideo {
                profile, target_codec, container, resolved_profile,
            } = operation
            {
                let resolved = crate::transcode::resolve::resolve_video_profile_ref(
                    &cp.video_profiles, profile,
                ).await?; // unknown name -> CONFIG_INVALID, no plan
                *target_codec = resolved.profile.target_codec.clone();
                *container = resolved.output_container.clone();
                *resolved_profile = Some(resolved.profile);
            }
        }
    }
    Ok(())
}
```

(Match the exact field names of `CompiledPolicy`/`CompiledPhase` as they exist — both are `pub Vec<...>`, verified at `compiled.rs:16,65`.) `resolve_profiles_in_policy` is async and needs the registry, so it can only be wired where a `&ControlPlane` is in scope. **Wire it into the two store-backed plan entry points (both `&self` methods on `ControlPlane`):**
1. The execute path — `compliance.rs::generate_compliance_report`, after the deserialize + `source_hash`/`schema_version` identity check (`compliance.rs:165-194`) and before `plan_compiled_policy_with_input`.
2. The stored-policy dry-run path — `plan_accepted_policy_version_with_input_set` (`plans.rs:52`, which `voom plan show` calls), at the same point before the planner.

At each insertion site bind the policy as `let mut policy = ...` so the `&mut` resolver call type-checks. The in-memory mutation is never re-serialized to `compiled_json`, so it cannot affect the stored identity check (which runs first). `plan_compiled_policy_with_input` itself stays **pure/sync** (planner-only; it assumes `resolved_profile` is already populated) — every caller resolves first. These two store-backed paths share `resolve_profiles_in_policy`, giving the dry-run/execute parity the spec §Resolution requires; **the spec's dry-run parity acceptance test (unknown-name rejection, planned/blocked decisions identical to execute) targets `plan_accepted_policy_version_with_input_set`**, not the offline path below.

**The offline `voom plan <file> <fixture>` developer path is store-free** (`cli/plan.rs::dry_run` opens no `ControlPlane` and calls the sync `plan_policy_source_with_input`; verified `plan.rs:15-64`). It therefore cannot read the `video_profiles` registry. Handle it honestly with a **sync, inline-only** resolver added to `resolve.rs`:

```rust
/// Resolves only `Inline` profiles (no registry needed). A `Named` reference
/// returns CONFIG_INVALID directing the operator to a store-backed plan, rather
/// than crashing the planner on a `None` `resolved_profile`.
///
/// # Errors
/// Returns `CONFIG_INVALID` for any `Named` reference or invalid inline settings.
pub fn resolve_inline_profiles_in_policy(
    policy: &mut voom_policy::CompiledPolicy,
) -> Result<(), VoomError> {
    for phase in &mut policy.phases {
        for operation in &mut phase.operations {
            if let voom_policy::CompiledOperation::TranscodeVideo {
                profile, target_codec, container, resolved_profile,
            } = operation
            {
                match profile {
                    voom_policy::VideoProfileRef::Inline(settings) => {
                        let typed = inline_to_worker_profile(settings)?;
                        *target_codec = typed.target_codec.clone();
                        *container = settings.output_container.clone()
                            .unwrap_or_else(|| "mkv".to_owned());
                        *resolved_profile = Some(typed);
                    }
                    voom_policy::VideoProfileRef::Named(name) => {
                        return Err(VoomError::Config(format!(
                            "named video profile `{name}` cannot be resolved offline; \
                             use `voom plan show` against an initialized store"
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}
```

Call `resolve_inline_profiles_in_policy` inside `plan_policy_source_with_input` (it is in the same crate, sync) before invoking the pure planner. Document in the plan/closeout that named-profile registry resolution is a store-backed capability and the offline fixture-planner supports inline profiles only. (This is a small, defensible deviation from the spec's literal "`plan_compiled_policy_with_input` resolves" wording; it honors the spec's intent — one resolution path, full parity for the store-backed plan/execute paths.)

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p voom-control-plane`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-control-plane/src/cases/compliance.rs crates/voom-control-plane/src/cases/plans.rs crates/voom-control-plane/src/transcode/resolve.rs
git commit -m "feat(control-plane): resolve profiles at planning-input assembly"
```

### Task 6.4: `copy_video` decision + dispatch consumes resolved profile

`dispatch::request_for` currently hardcodes `default_hevc()`. Thread the resolved profile and the control-plane-computed `copy_video` flag through. The decision: `copy_video = true` only when the profile is `copy_compatible`, the node is planned for a container-only change, and the source video stream already satisfies target codec, dimension caps, constrained pixel format, and constrained codec profile/level.

**Files:**
- Modify: `crates/voom-control-plane/src/transcode/dispatch.rs`
- Modify: `crates/voom-control-plane/src/transcode/mod.rs`
- Modify: `crates/voom-control-plane/src/transcode/resolve.rs` (add `decide_copy_video`)
- Modify: `crates/voom-control-plane/src/transcode/dispatch_test.rs` / `mod_test.rs`
- Modify: `crates/voom-control-plane/src/workflow/binding.rs` (carry resolved profile + container in the ticket payload)

- [ ] **Step 1: Write the failing tests** — in `resolve_test.rs` add `decide_copy_video` cases; in `mod_test.rs` extend the existing `execute_*` tests to assert (a) the dispatched request carries the resolved profile (not `default_hevc()`), (b) `copy_video` matches the snapshot, (c) a worker result whose `copied_video` disagrees with requested `copy_video` is rejected before commit.

```rust
#[test]
fn copy_video_true_only_for_container_change_when_conforming() {
    let profile = profile_hevc_mp4_copy_compatible();
    // source already hevc, within caps, matching pixfmt, only container differs
    assert!(decide_copy_video(&profile, &source_conforming_hevc_mkv()));
    // source needs re-encode (wrong codec) -> false
    assert!(!decide_copy_video(&profile, &source_h264_mkv()));
    // profile not copy_compatible -> false
    assert!(!decide_copy_video(&profile_hevc_mp4_no_copy(), &source_conforming_hevc_mkv()));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-control-plane copy_video`
Expected: FAIL.

- [ ] **Step 3: Implement**

In `resolve.rs` add `decide_copy_video(profile: &TranscodeVideoProfile, source: &SourceVideoFacts) -> bool` implementing the rule above (reuse the same observable comparisons the planner uses; factor the comparison helpers into a shared module if convenient, otherwise duplicate the small predicate with a comment cross-referencing the planner).

Change `dispatch::request_for` signature to accept `&ResolvedProfile` (or `&TranscodeVideoProfile` + container) and `copy_video: bool`:

```rust
pub fn request_for(
    selected: &SelectedSource,
    resolved: &ResolvedProfile,
    copy_video: bool,
    staging_root: &Path,
    staging_path: &Path,
) -> Result<TranscodeVideoRequest, VoomError> {
    Ok(TranscodeVideoRequest {
        input: /* unchanged */,
        output: TranscodeVideoOutput {
            staging_root: staging_root.to_string_lossy().into_owned(),
            path: staging_path.to_string_lossy().into_owned(),
            container: resolved.output_container.clone(),
            video_codec: resolved.profile.target_codec.clone(),
            overwrite: false,
        },
        profile: resolved.profile.clone(),
        copy_video,
    })
}
```

**Profile propagation chain (precise — the review flagged this was vague):** the resolved profile travels node payload → ticket payload → executor; it is NOT read from `CompiledOperation` at execute time. Concretely:
1. `workflow/binding.rs::render_policy_transcode_payload(operation_payload, ...)` reads the planner node payload's `resolved_profile` object and `container` (the pinned schema from Task 5.2) and copies both into the ticket payload it builds. Keep the existing `required_string(operation_payload, "profile")` call — `"profile"` is still a bare string (the resolved name) per the pinned schema, so it does not break. Add a typed read of the `resolved_profile` object (deserialize into `voom_worker_protocol::TranscodeVideoProfile`, error via `BindingError` if absent/malformed) and write it to the ticket payload.
2. `transcode/mod.rs::execute_transcode_video_with_dispatchers` deserializes `resolved_profile` + `container` from the ticket payload into a `ResolvedProfile`, re-reads the source snapshot, computes `copy_video` via `decide_copy_video`, and passes both to `request_for`. (Add the corresponding fields to `ExecuteTranscodeVideoInput` / the ticket-payload parse so they thread through — verify `ExecuteTranscodeVideoInput` in `transcode/mod.rs` and extend it.)
3. `dispatch::validate_result` additionally rejects when `result.copied_video != copy_video`, and validates `output_container`/`output_video_codec`/`output_width<=cap`/`output_height<=cap`/`output_pixel_format` against the request.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p voom-control-plane`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-control-plane/src/transcode/ crates/voom-control-plane/src/workflow/binding.rs
git commit -m "feat(control-plane): dispatch resolved profile and decide copy_video"
```

### Task 6.5: Profile-identity target-path naming

Sprint 12 names output `<stem>.hevc.mkv`. Sprint 15 names `<stem>.<profile-id>.<codec>.<ext>` so distinct-quality outputs coexist; a second run of the same profile collides → `CONFIG_INVALID`.

**Files:**
- Modify: `crates/voom-control-plane/src/transcode/stage.rs`
- Modify: `crates/voom-control-plane/src/transcode/stage_test.rs` (or sibling)

- [ ] **Step 1: Write the failing test**:

```rust
#[test]
fn target_file_name_includes_profile_identity_codec_and_container() {
    assert_eq!(
        output_file_name("/lib/Movie.mkv", "hevc-archive", "hevc", "mkv"),
        "Movie.hevc-archive.hevc.mkv"
    );
    assert_eq!(
        output_file_name("/lib/Movie.mkv", "inline-ab12cd34ef56", "av1", "mp4"),
        "Movie.inline-ab12cd34ef56.av1.mp4"
    );
}

#[test]
fn profile_identity_is_sanitized_for_filenames() {
    // No path separators or spaces survive into the file name.
    let name = output_file_name("/lib/Movie.mkv", "weird/name here", "hevc", "mkv");
    assert!(!name.contains('/'));
    assert!(!name.contains(' '));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-control-plane target_file_name`
Expected: FAIL — `output_file_name` takes one arg and hardcodes `.hevc.mkv`.

- [ ] **Step 3: Implement** — change `output_file_name` to take `(source, profile_id, codec, container)`, sanitize `profile_id` (replace any char outside `[A-Za-z0-9._-]` with `-`), and format `"{stem}.{profile_id}.{codec}.{container}"`. Update `target_path` to thread these from the resolved profile. Built-in names like `default-hevc` and `inline-<hash>` already pass through cleanly.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p voom-control-plane`
Expected: PASS.

- [ ] **Step 5: Phase gate + commit**

```bash
just ci
git add crates/voom-control-plane/src/transcode/stage.rs crates/voom-control-plane/src/transcode/stage_test.rs
git commit -m "feat(control-plane): name transcode targets by profile identity"
```

---

# Phase 7 — FFmpeg Worker: Per-Encoder Commands, MP4, Downscale, Copy, Validation, Preflight

Depends on Phase 1 only (can proceed in parallel with Phases 2–6). Extends the worker to build per-encoder command shapes, mux MKV/MP4, downscale aspect-preserving/downscale-only, honor `copy_video`, validate output dims/pixel-format, and preflight the specific encoder.

### Task 7.1: Per-encoder command shape builder

Replace the single libx265 builder with a function that branches on `profile.encoder` and emits the spec §6 command shapes from typed fields only.

**Files:**
- Modify: `crates/voom-ffmpeg-worker/src/ffmpeg.rs`
- Modify: `crates/voom-ffmpeg-worker/src/ffmpeg_test.rs`

- [ ] **Step 1: Write the failing golden tests** — use the existing arg-capture harness (writes ffmpeg args to a file). Add per-encoder + per-field assertions:

```rust
#[test]
fn libx265_command_uses_named_preset_and_optional_flags() {
    let args = capture_args(profile_x265_main10(), output_mkv()); // crf 18, preset slow, profile main10, pix yuv420p10le
    assert!(args.contains("-c:v\nlibx265\n"));
    assert!(args.contains("-crf\n18\n"));
    assert!(args.contains("-preset\nslow\n"));
    assert!(args.contains("-profile:v\nmain10\n"));
    assert!(args.contains("-pix_fmt\nyuv420p10le\n"));
    assert!(args.contains("-f\nmatroska\n"));
}

#[test]
fn libsvtav1_command_uses_numeric_preset() {
    let args = capture_args(profile_svtav1(), output_mp4()); // crf 32, preset 8
    assert!(args.contains("-c:v\nlibsvtav1\n"));
    assert!(args.contains("-crf\n32\n"));
    assert!(args.contains("-preset\n8\n"));
    assert!(args.contains("-f\nmp4\n"));
    assert!(args.contains("-tag:v\nav01\n"));
}

#[test]
fn libaom_command_sets_cpu_used_and_bitrate_zero() {
    let args = capture_args(profile_libaom(), output_mkv()); // crf 20, preset(cpu-used) 4
    assert!(args.contains("-c:v\nlibaom-av1\n"));
    assert!(args.contains("-crf\n20\n"));
    assert!(args.contains("-b:v\n0\n"));
    assert!(args.contains("-cpu-used\n4\n"));
}

#[test]
fn mp4_hevc_tags_hvc1() {
    let args = capture_args(profile_x265_main10(), output_mp4());
    assert!(args.contains("-tag:v\nhvc1\n"));
    assert!(args.contains("-f\nmp4\n"));
}

#[test]
fn downscale_applies_only_when_source_exceeds_cap() {
    // source 3840x2160, cap 1920x1080 -> scale filter present, even dims
    let args = capture_args_with_source(profile_1080p(), source_2160(), output_mp4());
    assert!(args.contains("-vf\n"));
    assert!(args.iter_args().any(|a| a.contains("scale=") && a.contains("min(")));
    // source already within cap -> no scale filter
    let args2 = capture_args_with_source(profile_1080p(), source_720(), output_mp4());
    assert!(!args2.contains("-vf\n"));
}

#[test]
fn copy_video_emits_stream_copy() {
    let args = capture_args_copy(profile_x265(), output_mp4(), /* copy_video */ true);
    assert!(args.contains("-c:v\ncopy\n"));
    assert!(!args.contains("-c:v\nlibx265\n"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-ffmpeg-worker libx265_command`
Expected: FAIL — only the Sprint 12 shape exists.

- [ ] **Step 3: Implement** — in `ffmpeg.rs`, factor the video-codec args into `fn video_codec_args(profile, copy_video) -> Vec<OsString>`:

- if `copy_video`: `["-c:v", "copy"]`.
- else branch on `profile.encoder`:
  - `libx265`: `-c:v libx265 -crf <n> -preset <named>` + optional `-tune`, `-profile:v`, `-level`, `-pix_fmt`.
  - `libsvtav1`: `-c:v libsvtav1 -crf <n> -preset <0-13>` + optional `-profile:v`, `-pix_fmt`; tune/level via `-svtav1-params tune=..:level=..`.
  - `libaom-av1`: `-c:v libaom-av1 -crf <n> -b:v 0 -cpu-used <0-8>` + optional `-tune`, `-profile:v`, `-pix_fmt`.

Container/format args via `fn container_args(container, codec) -> Vec<OsString>`:
- `mkv` → `-f matroska`.
- `mp4` → `-f mp4` plus `-tag:v hvc1` (hevc) or `-tag:v av01` (av1).

Downscale via `fn scale_args(profile, src_w, src_h) -> Vec<OsString>`: only when `src_w > max_width || src_h > max_height`; emit an aspect-preserving, downscale-only, even-dimension filter, e.g.:

```rust
// downscale-only, preserve aspect, force even dims
let vf = format!(
    "scale='min({cap_w},iw)':'min({cap_h},ih)':force_original_aspect_ratio=decrease,\
     scale=trunc(iw/2)*2:trunc(ih/2)*2"
);
vec![OsString::from("-vf"), OsString::from(vf)]
```

(The worker learns `src_w`/`src_h` from its own input probe — see Task 7.2.) Keep the existing `-map`/audio-copy/`-map_metadata`/`-progress` scaffolding.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p voom-ffmpeg-worker ffmpeg`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-ffmpeg-worker/src/ffmpeg.rs crates/voom-ffmpeg-worker/src/ffmpeg_test.rs
git commit -m "feat(ffmpeg): per-encoder command shapes, mp4 mux, downscale, copy"
```

### Task 7.2: Input probe, copy precondition revalidation, output validation

The worker probes its input to learn source dimensions (for downscale) and, when `copy_video`, to revalidate the source still satisfies codec/dims/pixfmt/profile/level before emitting `-c:v copy` (fail loudly otherwise). Output validation extends to dimensions and pixel format.

**Files:**
- Modify: `crates/voom-ffmpeg-worker/src/ffmpeg.rs` (input probe + output validation)
- Modify: `crates/voom-ffmpeg-worker/src/handler.rs` (copy precondition gate, contract checks)
- Modify: `crates/voom-ffmpeg-worker/src/handler_test.rs`

- [ ] **Step 1: Write the failing tests** — in `handler_test.rs`:

```rust
#[tokio::test]
async fn copy_video_with_nonconforming_source_fails_loudly() {
    // request copy_video=true but input probe shows h264 (not target hevc)
    let err = run_transcode(copy_request_hevc(), fixture_h264_mkv()).await.unwrap_err();
    assert!(matches!(err, TranscodeVideoError::MalformedWorkerResult { .. }
        | TranscodeVideoError::ConfigInvalid { .. }));
}

#[tokio::test]
async fn output_dimensions_exceeding_cap_is_malformed_result() {
    // forced via a stub probe returning 4000-wide output under a 1920 cap
    // (covered by output-validation unit test)
}

#[tokio::test]
async fn mp4_output_contract_now_accepted() {
    // previously rejected; now mp4 is a supported container
    let res = run_transcode(mp4_av1_request(), fixture_small_av1_source()).await;
    assert!(res.is_ok());
}
```

(Conformance tests that actually run ffmpeg per encoder go in `tests/` — see Task 7.4. Here keep the pre-ffmpeg contract + revalidation logic unit-testable with stubbed probes where possible.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-ffmpeg-worker copy_video_with_nonconforming`
Expected: FAIL.

- [ ] **Step 3: Implement**

- Add `probe_input(config, path) -> InputProbe { width, height, codec, pixel_format, codec_profile, codec_level, video_stream_count }` mirroring `probe_output` but for the source.
- In `handle_transcode_video`: after input pre-observation, run `probe_input`. Pass `width`/`height` to the command builder for downscale.
- When `request.copy_video`: assert the input probe satisfies `target_codec`, dimension caps, constrained `pixel_format`, constrained `codec_profile`/`codec_level`; on any mismatch return `MalformedWorkerResult` (the control plane decided wrongly or the source drifted) — never silently re-encode or copy a non-conforming stream.
- Remove the Sprint 12 "mp4 output rejected" contract check (mp4 is now supported); keep rejecting unknown containers/codecs.
- Extend `probe_output` to also read width/height/pix_fmt and validate: container matches request, codec matches request, `width <= max_width`/`height <= max_height` when capped, `pixel_format` matches when constrained. On mismatch → `OutputFactsMismatch` → `MalformedWorkerResult`.
- Populate `TranscodeVideoResult.output_width/height/pixel_format` from the output probe and `copied_video` from `request.copy_video`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p voom-ffmpeg-worker`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-ffmpeg-worker/src/ffmpeg.rs crates/voom-ffmpeg-worker/src/handler.rs crates/voom-ffmpeg-worker/src/handler_test.rs
git commit -m "feat(ffmpeg): input probe, copy revalidation, output dim/pixfmt validation"
```

### Task 7.3: Per-encoder preflight

Preflight must confirm the specific encoder a job needs. Sprint 12 only checked `libx265`. Extend to detect `libx265`, `libsvtav1`, `libaom-av1`, and the `mp4` muxer; a missing encoder is a loud `ExternalSystemUnavailable` setup failure.

**Files:**
- Modify: `crates/voom-ffmpeg-worker/src/preflight.rs`
- Modify: `crates/voom-ffmpeg-worker/src/preflight_test.rs`

- [ ] **Step 1: Write the failing tests**:

```rust
#[test]
fn preflight_detects_all_three_video_encoders() {
    let report = preflight_with_paths(&fake_ffmpeg_all_encoders(), &fake_ffprobe()).unwrap();
    assert!(report.has_encoder("libx265"));
    assert!(report.has_encoder("libsvtav1"));
    assert!(report.has_encoder("libaom-av1"));
    assert!(report.has_muxer("mp4"));
}

#[test]
fn preflight_rejects_missing_libsvtav1() {
    let err = preflight_with_paths(&fake_ffmpeg_without("libsvtav1"), &fake_ffprobe());
    assert!(err.is_err());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-ffmpeg-worker preflight_detects_all_three`
Expected: FAIL.

- [ ] **Step 3: Implement** — extend `FfmpegPreflight` to record the three video encoders and the `mp4` muxer; extend the `-encoders`/`-muxers` parse to require `libx265`, `libsvtav1`, `libaom-av1`, `matroska`, `mp4` (keep `aac`/`libopus`/`ogg`). Add `has_encoder`/`has_muxer` accessors. Failure for any missing → `FFmpegPreflightError` mapped to `ExternalSystemUnavailable` with explicit diagnostics.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p voom-ffmpeg-worker preflight`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-ffmpeg-worker/src/preflight.rs crates/voom-ffmpeg-worker/src/preflight_test.rs
git commit -m "feat(ffmpeg): preflight all three video encoders and mp4 muxer"
```

### Task 7.4: Per-encoder ffmpeg conformance tests

Real-ffmpeg conformance per encoder. These require the CI ffmpeg build to provide `libx265`, `libsvtav1`, `libaom-av1`; a missing encoder is a **setup failure, not a skipped test** (spec §10).

**Files:**
- Modify: `crates/voom-ffmpeg-worker/tests/transcode_worker.rs` (or a new `tests/transcode_conformance.rs`)

- [ ] **Step 1: Write conformance tests** — for each encoder family, with small fixture media: success + correct output codec/container; MKV and MP4 mux (`hvc1`/`av01`); max-dimension downscale; `copy_video` copy path; worker fails loudly when `copy_video=true` but input is non-conforming; pixel-format conversion; output validation mismatches (codec/container/dims/pixfmt); missing input; input drift; existing output; bad payload; path escape; provider failure; timeout. Assert via `preflight_from_process_env()` at test start so a missing encoder **fails** the test loudly rather than skipping.

- [ ] **Step 2: Run** — `cargo test -p voom-ffmpeg-worker --test transcode_conformance`. Requires a real ffmpeg with the three encoders. Expected: PASS (or a loud setup failure naming the missing encoder).

- [ ] **Step 3: Phase gate + commit**

```bash
just ci
git add crates/voom-ffmpeg-worker/tests/
git commit -m "test(ffmpeg): per-encoder transcode conformance"
```

---

# Phase 8 — CLI Inspection, Report Facts, Integration, Closeout

Depends on all prior phases. Adds `voom profile list`/`show`, extends transcode execution reports and events with resolved-profile facts, adds the end-to-end integration test, and writes the closeout matrix.

### Task 8.1: `voom profile list` and `voom profile show <name>`

**Files:**
- Modify: `crates/voom-cli/src/cli.rs` (add `Profile(ProfileCommand)` + enum)
- Modify: `crates/voom-cli/src/main.rs` (`dispatch_profile`)
- Modify: `crates/voom-cli/src/commands/mod.rs` (`pub mod profile;`)
- Create: `crates/voom-cli/src/commands/profile.rs`
- Modify: `crates/voom-control-plane/src/lib.rs` (add `list_video_profiles` / `get_video_profile` use-case methods)
- Create snapshots under `crates/voom-cli/tests/snapshots/`

- [ ] **Step 1: Write the failing snapshot test** — add `crates/voom-cli/tests/profile_envelope.rs`:

```rust
#[tokio::test]
async fn profile_list_emits_seeded_builtins() {
    let seeded = seed().await; // init + ephemeral DB
    let out = profile_command(&seeded.url).args(["list"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let mut json = envelope(out.stdout);
    assert_eq!(json["command"], "profile");
    assert_eq!(json["status"], "ok");
    redact_local(&mut json);
    insta::assert_json_snapshot!("profile_list", json);
}

#[tokio::test]
async fn profile_show_unknown_is_not_found() {
    let seeded = seed().await;
    let out = profile_command(&seeded.url).args(["show", "--name", "nope"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let mut json = envelope(out.stdout);
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "NOT_FOUND");
    redact_local(&mut json);
    insta::assert_json_snapshot!("profile_show_unknown", json);
}

#[tokio::test]
async fn profile_show_emits_full_profile() {
    let seeded = seed().await;
    let out = profile_command(&seeded.url).args(["show", "--name", "hevc-archive"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let mut json = envelope(out.stdout);
    redact_local(&mut json);
    insta::assert_json_snapshot!("profile_show_hevc_archive", json);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-cli profile_envelope`
Expected: FAIL — `profile` subcommand missing.

- [ ] **Step 3: Implement**

In `cli.rs`:

```rust
#[command(subcommand)]
Profile(ProfileCommand),
```

```rust
#[derive(Subcommand, Debug, Clone)]
pub enum ProfileCommand {
    List,
    Show {
        #[arg(long)]
        name: String,
    },
}
```

Add `list_video_profiles(&self) -> Result<Vec<VideoProfile>, VoomError>` and `get_video_profile(&self, name) -> Result<Option<VideoProfile>, VoomError>` to `ControlPlane` delegating to `self.video_profiles`.

Create `commands/profile.rs` mirroring `commands/node.rs`: `run(database_url, local, command)`, a `list` handler emitting a `{ profiles: [...] }` data struct, a `show` handler emitting the single profile or `emit_err("profile", "NOT_FOUND", ...)` returning exit code 2. Serialize all profile fields (spec §8). Wire `dispatch_profile` in `main.rs` like `dispatch_node`.

- [ ] **Step 4: Run + review snapshots**

Run: `cargo test -p voom-cli profile_envelope` then `cargo insta review` to accept the new snapshots.
Expected: PASS after accepting.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-cli/src/cli.rs crates/voom-cli/src/main.rs crates/voom-cli/src/commands/ crates/voom-cli/tests/ crates/voom-control-plane/src/lib.rs
git commit -m "feat(cli): add voom profile list and show"
```

### Task 8.2: Extend transcode reports + events with profile facts

Spec §8: transcode events gain resolved profile name, encoder, target codec, output container; success payloads add `copied_video` + observed output dims/pixel-format. CLI transcode execution reports expose the resolved profile facts alongside the existing IDs.

**Files:**
- Modify: `crates/voom-control-plane/src/transcode/events.rs` (payload fields)
- Modify: the transcode execution report data struct (in `voom-control-plane` and/or `voom-cli`)
- Modify: the relevant event/report test + CLI snapshot tests

- [ ] **Step 1: Write the failing tests** — extend the existing transcode event-payload test to assert the new fields on started/succeeded/failed payloads; extend the CLI transcode-report snapshot to include `resolved_profile`, `encoder`, `target_codec`, `output_container`, `copied_video`, `output_width/height/pixel_format`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p voom-control-plane transcode_event` and `cargo test -p voom-cli` (report snapshot)
Expected: FAIL — fields absent.

- [ ] **Step 3: Implement** — add the fields to the event payload builders in `events.rs` (drawing name/encoder/target_codec/container from the resolved profile threaded through `ExecuteTranscodeVideoInput`, and the observed output facts + `copied_video` from `TranscodeVideoResult`). Add the same facts to the report data struct. Keep correlation data (job/ticket/lease/source/staging IDs) intact.

- [ ] **Step 4: Run + accept snapshots**

Run: `cargo test -p voom-control-plane` ; `cargo test -p voom-cli` ; `cargo insta review`.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-control-plane/src/transcode/events.rs crates/voom-cli/
git commit -m "feat(control-plane): report and event resolved-profile facts"
```

### Task 8.3: End-to-end integration test

Spec §10: scan → policy plan → execute → transcode → verify → commit → result snapshot, covering named and inline profiles, HEVC and AV1, MKV and MP4.

**Files:**
- Create: `crates/voom-control-plane/tests/video_profile_flow.rs` (model on the existing `tests/video_transcode_flow.rs`)

- [ ] **Step 1: Write the integration test** — parametrize over a small matrix using fixture media:
  - `transcode video to hevc` (named default, MKV) on an H.264 source → planned → executed → committed `<stem>.default-hevc.hevc.mkv`; re-plan result = NoOp.
  - `transcode video to hevc using profile "hevc-1080p"` (MP4, copy_compatible) on an oversized HEVC source → downscaled re-encode, MP4 output.
  - `transcode video to av1 { encoder: libsvtav1 crf: 32 preset: 8 output_container: mp4 }` (inline) on an H.264 source → AV1 MP4, target `<stem>.inline-<hash>.av1.mp4`.
  - assert the committed result `FileVersion`/`FileLocation`/`MediaSnapshot` exist and the snapshot reports the new codec/container/pixel-format.
  - Start with `preflight_from_process_env()` so a missing encoder fails loudly.

- [ ] **Step 2: Run**

Run: `cargo test -p voom-control-plane --test video_profile_flow`
Expected: PASS (requires real ffmpeg with the three encoders).

- [ ] **Step 3: Commit**

```bash
git add crates/voom-control-plane/tests/video_profile_flow.rs
git commit -m "test(control-plane): end-to-end named and inline profile flows"
```

### Task 8.4: Closeout matrix + documentation scan

**Files:**
- Create: `docs/superpowers/specs/2026-05-28-voom-sprint-15-closeout.md`

- [ ] **Step 1: Write the closeout** — model on `2026-05-24-voom-sprint-10-closeout.md`. Include an evidence matrix mapping each spec §11 acceptance criterion to the test(s) and command that prove it (profile model/migration, descriptor validation, resolution incl. unknown-name rejection, dry-run parity, inline identity, planner compliance + MP4 gating + notes, protocol serialization + command-line invariance, legacy bare-string round-trip, per-encoder conformance, command-shape goldens, preflight, dispatch consuming the resolved profile, copy_video agreement, target-path discriminator, CLI snapshots, end-to-end integration).

- [ ] **Step 2: Documentation placeholder scan** — grep the repo for stale Sprint-12 single-profile claims and any TODO/placeholder text introduced this sprint:

Run: `rg -n "default_hevc|hevc\\.mkv|only .* hevc" docs crates --glob '!**/specs/**'` and reconcile any doc text that still asserts single-profile behavior.

- [ ] **Step 3: Final full CI**

Run: `just ci`
Expected: PASS — `fmt-check`, `lint`, `check-test-layout`, `test`, `doc`, `deny`, `audit` all green.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-05-28-voom-sprint-15-closeout.md
git commit -m "docs: add Sprint 15 closeout evidence matrix"
```

---

## Self-Review (against the spec)

**Spec coverage — every §2 scope item maps to a task:**
- DSL generalization (hevc/av1, bare→default) → 3.2–3.4.
- Durable `video_profiles` table + seed → 2.1; read-only repo → 2.2.
- Three encoder bindings + per-encoder validation → 1.2, 1.4, 3.3.
- Compiler emits typed `VideoProfileRef`, rejects all invalid-inline classes → 3.1, 3.3, 3.4.
- Control-plane resolution (unknown → diagnostic before planning) → 6.2, 6.3.
- Dimension/pixfmt/container-aware compliance + resource estimates → 5.2.
- ffprobe normalizer + projection extension (pixfmt/profile/level, per-stream kind+codec) → 4.1, 4.2.
- MKV + MP4 output, MP4 incompat blocking → 5.2 (gate), 7.1 (mux).
- `copy_compatible`/`copy_video` (control plane decides, worker validates) → 6.4, 7.2.
- Worker protocol `TranscodeVideoProfile`/result extension + `copy_video` → 1.3.
- Worker per-encoder commands, downscale, copy short-circuit, output validation → 7.1, 7.2.
- Worker per-encoder preflight → 7.3.
- Dispatch consumes resolved profile → 6.4.
- CLI `profile list`/`show` + report facts → 8.1, 8.2.
- Closeout evidence → 8.4.
- Compiled-policy backward compatibility (legacy bare-string) → 3.1 + 3.4 round-trip test.
- `inline-<hash>` canonical, version-stable identity → 5.1.

**Type-consistency checks:**
- `TranscodeVideoProfile` field set is defined once (1.3) and reused by store `to_worker_profile` (2.2), resolver (6.2), planner (5.2), worker (7.1).
- `VideoProfileRef`/`VideoProfileSettings` defined in 3.1; consumed by compiler (3.3/3.4), resolver (6.2). Field names (`output_container`, `copy_compatible`, `codec_profile`, `codec_level`) match the migration columns (2.1) and protocol fields (1.3).
- The planner reads the `resolved_profile: Option<TranscodeVideoProfile>` field on `CompiledOperation::TranscodeVideo` (defined in Task 3.4, populated in Task 6.3, consumed in Task 5.2). This is the single pinned Phase 5↔6 contract; there is no payload-JSON variant. The field is `#[serde(default, skip_serializing_if = "Option::is_none")]` so it never enters `compiled_json` and the legacy bare-string compatibility holds.
- `inline_profile_id` is the single identity source (5.1), used for the profile `name` (6.2) and target-path discriminator (6.5).

**Known cross-phase coupling to watch during execution:**
- Phase 5 is unit-tested with hand-built `resolved_profile: Some(...)` values before Phase 6 wires real resolution. The contract is the `resolved_profile` field added in Task 3.4 — pinned, typed, no longer ambiguous.
- Codec-profile/level token normalization (ffprobe `"Main 10"` ↔ profile `main10`) is needed in both the planner (5.2) and worker copy revalidation (7.2). Implement one normalization helper and reference it from both, or duplicate with a cross-reference comment (AGENTS.md Rule 3: don't abstract prematurely, but Rule 7: surface the duplication).

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-05-28-voom-sprint-15-implementation.md`.** Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration. REQUIRED SUB-SKILL: `superpowers:subagent-driven-development`. Note: parallel subagents must each work in their own worktree (`wt switch <branch>`), never the shared checkout.

2. **Inline Execution** — execute tasks in this session via `superpowers:executing-plans`, batched with checkpoints for review.

Either way: phases are dependency-ordered (P1 → P2/P4 → P3 → P5 → P6 → P7 → P8; P7 may run in parallel after P1). Finish each phase with `just ci` green before the next. Work stays on `feat/sprint-15`; no PR until the whole sprint lands.
