# Issue #409 — Linux VAAPI video acceleration design

Status: accepted
Date: 2026-07-29
Issue: #409
ADR: [0051 — VAAPI device identity is the PCI address, and capability is probe-proven](../../adr/0051-vaapi-device-identity-and-probe-proven-capability.md)
Builds on: [ADR 0049](../../adr/0049-accelerator-devices-are-worker-resources.md), issue #400

## 1. Scope

Extend the ADR 0049 typed accelerator-resource model to AMD VAAPI, proven on the
acceptance host recorded in §2.

In scope:

- `hevc_vaapi` encode, Main (8-bit) and Main10 (10-bit).
- Typed `vaapi` decode mode covering `h264`, `hevc`, `av1`.
- PCI-address device identity, worker binding, and probe-proven capability advertisement.
- Compatible-device scheduling, per-device capacity, claim recovery, preflight,
  deterministic command generation, durable assignment evidence.

Out of scope, deferred to follow-up issues with the same hardware available:

- `h264_vaapi` and `av1_vaapi` encode (both proven on the host; see §9).
- Intel VAAPI. Issue #409 requires one tested vendor per slice; this is AMD/`radeonsi`.
- `rc_mode` values other than CQP; `codec_level` for VAAPI.

Unchanged: every existing software and NVIDIA profile, command shape, output-fact
verification, artifact commit, event, and report contract.

## 2. Acceptance system

The record issue #409's "Required development system" clause demands.

| Field | Value |
|---|---|
| OS | Fedora Linux 44 (Workstation Edition) |
| Kernel | 7.1.5-200.fc44.x86_64 |
| GPU | AMD Radeon 8060S Graphics (Strix Halo APU) |
| PCI address | `0000:f4:00.0` |
| PCI IDs | vendor `0x1002`, device `0x1586`, rev `0xc1`, subsystem `0x1f4c:0xb026` |
| DRM render node | `/dev/dri/renderD128` |
| Stable node path | `/dev/dri/by-path/pci-0000:f4:00.0-render` |
| Render nodes present | 1 |
| Kernel driver | `amdgpu` |
| VA driver | Mesa Gallium 26.1.5 `radeonsi` (strix_halo, ACO, DRM 3.64) |
| FFmpeg | 8.1.2, `--enable-vaapi --enable-libdrm` |

### 2.1 Driver-build dependency

`vainfo` encode entrypoints, isolated with an explicit `LIBVA_DRIVERS_PATH`:

- **stock** `mesa-dri-drivers` (`/usr/lib64/dri`) — `VAProfileAV1Profile0: VAEntrypointEncSlice`, and nothing else.
- **freeworld** `mesa-va-drivers-freeworld` 26.1.5-2.fc44, RPM Fusion (`/usr/lib64/dri-freeworld`) —
  `VAProfileH264ConstrainedBaseline`, `VAProfileH264Main`, `VAProfileH264High`,
  `VAProfileHEVCMain`, `VAProfileHEVCMain10`, `VAProfileAV1Profile0`, all `VAEntrypointEncSlice`.

So `hevc_vaapi` encode requires the freeworld build. Installing that package puts
`/usr/lib64/dri-freeworld/` on the global library path via
`ld.so.conf.d/mesa-freeworld-lib64.conf` and makes it the **system-wide default**:
with `LIBVA_DRIVERS_PATH` unset, `LD_DEBUG=libs` confirms FFmpeg loads
`/usr/lib64/dri-freeworld/radeonsi_drv_video.so`. Reverting requires an explicit
`LIBVA_DRIVERS_PATH=/usr/lib64/dri`.

This is the observation that forces §5: capability tracks the loaded driver build,
which is invisible from the render node, from FFmpeg's encoder list, and from
`vainfo` run without an explicit path.

### 2.2 Measured capability

Encode, verified by executing and inspecting output:

| Encoder | Result |
|---|---|
| `hevc_vaapi` 8-bit | works; output `hevc / Main / yuv420p` |
| `hevc_vaapi` 10-bit | works via `format=p010`; output `Main 10 / yuv420p10le` |
| `h264_vaapi` | works (freeworld only) |
| `av1_vaapi` | works, including on stock; 1080p/60f in 0.23s |

Hardware decode for `h264`, `hevc`, `av1`, each verified with
`-hwaccel_output_format vaapi`, which errors rather than silently falling back.

`hevc_vaapi` option vocabulary, from `ffmpeg -h encoder=hevc_vaapi`:

- `-qp` int `0..52`; 53 rejected. 0 is the default and means auto.
- `-rc_mode`: `auto`(0) `CQP`(1) `CBR`(2) `VBR`(3) `ICQ`(4) `QVBR`(5) `AVBR`(6).
- `-profile` int (`general_profile_idc`, -99..255), **with named constants** —
  `main`(1), `main10`(2), `rext`(4). `-profile:v main10` and `-profile:v 2` both work;
  `-profile:v bogus` is rejected (`Undefined constant`).
- `-level` int (`general_level_idc`, -99..255), also with named constants —
  `1`(30), `2`(60), `2.1`(63), `3`(90), `3.1`(93), `4`(120), `4.1`(123), `5`(150),
  `5.1`(153), `5.2`(156), `6`(180), `6.1`(183), `6.2`(186). Note these spell the whole
  levels `4`/`5`/`6` where VOOM's vocabulary spells them `4.0`/`5.0`/`6.0`.
- `-tier` 0/1 (`main`(0), `high`(1)). `-async_depth` 1..64.
- No `-preset`, no `-compression_level`.

Verified profile/pixel-format pairings:

| Invocation | Result |
|---|---|
| `format=p010,hwupload` (no `-profile`) | `Main 10 / yuv420p10le` |
| `format=p010` + `-profile:v 2` | `Main 10 / yuv420p10le` |
| `format=p010` + `-profile:v main10` | `Main 10 / yuv420p10le` |
| `format=nv12,hwupload` (no `-profile`) | `Main / yuv420p` |
| `format=nv12` + `-profile:v 1` | `Main / yuv420p` |
| `format=p010` + `-profile:v main` | **fails** — `No usable encoding profile found` |

The last row matters: FFmpeg rejects a 10-bit surface under an 8-bit profile, so the
descriptor's `eight_bit_only_profiles` rule catches the same mistake earlier with a
better message rather than duplicating an unenforced invariant.

### 2.3 Fixture requirement

`testsrc` piped to `libx265` yields `gbrp` (4:4:4 RGB), which VAAPI cannot decode.
Test fixtures must be generated with an explicit `-pix_fmt yuv420p`. An early probe
round using `gbrp` sources produced misleading decode and pipeline failures.

## 3. Typed vocabulary (`voom-core`)

Extends `crates/voom-core/src/media/encoder_caps.rs`:

- `VideoEncoderBackend::Vaapi`.
- `QualityDomain::Qp { min: 1, max: 52 }` — 0 excluded because it means auto (§2.2).
- `PresetDomain::None`, for an encoder with no speed knob.
- `HEVC_VAAPI` descriptor:

| Field | Value |
|---|---|
| `encoder` | `hevc_vaapi` |
| `target_codec` | `hevc` |
| `quality_domain` | `Qp { min: 1, max: 52 }` |
| `backend` | `Vaapi` |
| `preset_domain` | `None` |
| `tunes` | `&[]` |
| `codec_profiles` | `&["main", "main10"]` |
| `codec_levels` | `&[]` (rejected — see ADR 0051 §4) |
| `pixel_formats` | `&["nv12", "p010"]` |
| `ten_bit_pixel_formats` | `&["p010"]` |
| `eight_bit_only_profiles` | `&["main"]` |
| `requires_bitrate_zero` | `false` |

- `VAAPI_VIDEO_DECODERS: &[&str]` — `["h264", "hevc", "av1"]`. A flat codec list, **not**
  `NVIDIA_VIDEO_DECODERS`' `(codec, decoder)` pairs: CUVID needs the pair because it has
  a distinct decoder name per codec (`hevc_cuvid`), whereas VAAPI decode is selected by
  `-hwaccel vaapi` and the codec's own decoder. A pair whose two elements are always the
  same string would be redundant structure copied for symmetry's sake.

### 3.1 Profile field changes

In `crates/voom-core/src/media/transcode_video_profile.rs`:

- `preset: String` becomes `preset: Option<String>`.
- New `qp: Option<u8>`.
- `VideoDecodeMode` gains `Vaapi(VaapiVideoDecode)`.
- **`is_nvidia()` must stop being `!self.is_software()`** (currently line 172). It
  gates validation at line 83 and would report `true` for VAAPI. Both predicates
  become explicit matches, and a new `is_vaapi()` joins them.

`validate_profile_against_descriptor` gains:

- A `Qp` arm in the quality-domain match, which is exhaustive over
  `(quality_domain, crf, cq, qp)`.
- Preset presence validation: `PresetDomain::None` requires `preset.is_none()`; every
  other domain requires `Some` and validates the value.
- A decode/encoder pairing rule mirroring the existing NVIDIA one: VAAPI decode
  requires a VAAPI encoder.

## 4. Device identity and worker binding

Configuration accepts a **PCI address** (`0000:f4:00.0`), never a render-node path or
ordinal. Startup:

1. Resolve `/dev/dri/by-path/pci-<addr>-render` to its render node.
2. Read the resolved node's PCI address back and compare to the configured value.
   A mismatch fails startup.
3. Advertise `hardware` token `vaapi:pci-<addr>`.

Because the PCI address cannot renumber while `renderD*` can, this satisfies issue
#409's "identity remains stable across render-node enumeration changes" branch rather
than its detect-and-reject fallback.

Binding strength comes from `-vaapi_device <node>` naming the device directly at open
time, not from step 2. VAAPI has no equivalent of the `CUDA_VISIBLE_DEVICES`-plus-device-zero
indirection that forces ADR 0049's PID-to-UUID readback, so step 2's narrower job is to
catch a stale or incorrect `by-path` symlink — udev generates that symlink from the very
address the check re-reads. Proof that an encode ran on the intended device comes from
the §5 probe. See ADR 0051 §1.

`sysfs unique_id` is deliberately not used: it exists only on discrete GPUs and is
absent on this APU.

## 5. Probe-proven capability

For each candidate codec, startup runs a **claim-owned smoke encode** on the bound
node and advertises the codec only on success. FFmpeg's `-encoders` list and `vainfo`
entrypoints are used only to skip obviously-absent codecs before probing; they never
substitute for a probe.

Encoders and decoders are probed separately, by different means:

- **Encoder probe** — synthesize frames with `lavfi testsrc`, upload, encode, require a
  non-empty output. No input fixture on disk is needed.
- **Decoder probe** — decode a small **bundled** fixture per codec with
  `-hwaccel vaapi -hwaccel_output_format vaapi`, which errors instead of silently
  falling back to software, and require a clean exit. The fixtures are committed
  4:2:0 `yuv420p` samples, one per codec in `VAAPI_VIDEO_DECODERS`; §2.3 is why
  `yuv420p` is mandatory and why they cannot be generated by the obvious
  `testsrc`-to-`libx265` recipe at build time.

The probe is not cached across restarts, because a host driver change can move
advertised capability with no VOOM configuration change (§2.1).

Probing is bounded by ADR 0049 §3's existing clocks, adopted unchanged: a per-probe
encode timeout, the one-minute capacity clock for the concurrent probe, and the
five-minute readiness deadline overall. Expiry fails startup naming the codec or
capacity that did not prove, so a hung probe cannot leave a worker pending. The cost
is real and paid per start — one encode per candidate codec plus `capacity`
concurrent encodes — and is recorded in ADR 0051's Consequences.

The advertised descriptor records: backend, PCI address, device name, usable
encoders, usable decoders, driver string, and tested capacity.

## 6. Capacity, scheduling, preflight

**Capacity** is operator-declared, bounded `1..=16`, defaulting to 1, and proven at
startup by that many concurrent smoke encodes. VAAPI exposes no session
enumeration, so a failed capacity probe is always reported as diagnostic
uncertainty — ADR 0049 §3's external-contention/VOOM-orphan distinction has no
VAAPI counterpart and never will.

**Scheduling** reuses ADR 0049's compatible-device selection, per-device leases,
deterministic worker-ID tie-breaking, and recovery window unchanged. A VAAPI
requirement matches only a live, identity-verified VAAPI descriptor.

**Preflight** requires an identity-verified `hevc_vaapi` descriptor; a `vaapi`-decode
profile additionally requires at least one usable VAAPI decoder. Per-file source
codec compatibility stays per-file. The existing split is reused unchanged: an
unsupported source codec becomes a planner-blocked file, while a recognized codec
absent from every live descriptor becomes a ticket-scoped `MissingCapability`.
Dispatch repeats identity validation before acquiring the lease.

Actionable preflight failures, each with a distinct diagnostic message:

| Condition | Detected by |
|---|---|
| PCI address malformed in config | config parse |
| PCI address unresolvable to a render node | `by-path` lookup miss |
| render node absent or not a device | `stat` on the resolved path |
| permission denied on the render node | `open` failure (worker not in `video`/`render` group) |
| PCI readback mismatch | §4 step 2 |
| driver build lacks the codec | probe encode — the observed `No usable encoding profile found` |
| probe encode fails for any other reason | probe encode |
| capacity probe fails | concurrent probe |
| probe or readiness clock expires | §5 clocks |

**Error codes are unchanged.** AGENTS.md makes `VoomError::code()` strings public
contract, and every condition above is a worker-preflight or capability failure that
already has a home in the existing taxonomy — the ticket-scoped `MissingCapability`
path and the existing worker-startup failure reporting. This slice adds no new `code`
string; it adds distinct human-readable messages under existing codes. Anything that
would need a new code is out of scope and a signal to revisit this decision.

## 7. Command generation

All three shapes were executed on the acceptance host.

| Path | Command core |
|---|---|
| software decode → hw encode | `-vaapi_device <node> -i IN -vf 'format=nv12,hwupload' -c:v hevc_vaapi -rc_mode CQP -qp N` |
| hw decode → hw encode | `-hwaccel vaapi -hwaccel_device <node> -hwaccel_output_format vaapi -i IN -c:v hevc_vaapi -rc_mode CQP -qp N` (no `-vf`) |
| Main10 | as above with `format=p010`, plus `-profile:v main10` when the profile sets `codec_profile` |

Rules:

- `-rc_mode CQP` is always emitted explicitly; `auto` is never relied on.
- Frame transfers are explicit. A software-decoded source uploads via
  `format=<fmt>,hwupload`; a VAAPI-decoded source stays in hardware frames with no
  filter inserted.
- `codec_profile` is passed through **by name** — `-profile:v main` / `-profile:v main10`
  — because `hevc_vaapi`'s `-profile` carries those named constants (§2.2). No
  name-to-integer mapping layer is needed, and FFmpeg rejects an unknown name.
- `-profile:v` is emitted only when the profile sets `codec_profile`. The pixel format
  alone already selects the right profile (§2.2), so emitting it unconditionally would
  invent a value the operator did not choose.
- `codec_level` is never emitted; the descriptor rejects it (§3).
- No software encoder is ever substituted. A failure is a failure.

## 8. Durable schema — migration `0030`

Table rebuild following `0029_nvidia_video_acceleration.sql`'s pattern
(`video_profiles_new` → copy → rename):

- Add `qp INTEGER`.
- `preset TEXT` (was `TEXT NOT NULL`).
- `CHECK (encoder IN ('libx265','libsvtav1','libaom-av1','hevc_nvenc','hevc_vaapi'))`.
- Quality CHECK extended so exactly one of `crf`/`cq`/`qp` is present per encoder,
  with `hevc_vaapi` requiring `qp BETWEEN 1 AND 52` and both others NULL.
- `CHECK ((encoder = 'hevc_vaapi' AND preset IS NULL) OR (encoder != 'hevc_vaapi' AND preset IS NOT NULL))`.
- `CHECK (decode_backend IN ('software','nvidia','vaapi'))`, plus per-backend encoder
  pairing so `nvidia` decode implies `hevc_nvenc` and `vaapi` decode implies `hevc_vaapi`.

Existing rows carry `preset IS NOT NULL` and `qp IS NULL`, so the copy is a
straight projection with `NULL AS qp`.

Per ADR 0013, `docs/payload-contract-inventory.md` and
`scripts/payload-contract-scope.txt` gain the new durable typed columns, and
payload evolution stays additive.

### 8.1 Worker-protocol change

In `crates/voom-worker-protocol/src/video_acceleration.rs`:

- Add `VaapiVideoAcceleratorDescriptor`, `VaapiVideoHardwareRequirement`,
  `VaapiVideoHardwareAssignment`, each `#[serde(deny_unknown_fields)]`.
- Add `Vaapi` variants to `VideoHardwareRequirement` and `VideoHardwareAssignment`.
- `LocalWorkerBound.accelerator` changes from
  `Option<NvidiaVideoAcceleratorDescriptor>` to an `Option` of a new tagged
  `VideoAcceleratorDescriptor` enum. This is **not** additive, so it is a coordinated
  binary-before-DB change per ADR 0013's contract.

## 9. Testing

- **Profile validation** — valid `qp` bounds (1 and 52 accepted, 0 and 53 rejected);
  `preset` present rejected for VAAPI and required for others; `codec_level` rejected
  for VAAPI; `main` + `p010` rejected as an 8-bit-only profile with a 10-bit format;
  VAAPI decode with a non-VAAPI encoder rejected.
- **Conformance** — `insta` snapshots pinning the exact argv for all three shapes in
  §7, 8-bit and 10-bit.
- **Scheduler** — compatible-device selection, per-device capacity exhaustion,
  deterministic tie-breaking, claim recovery, and no cross-device assignment. Unit
  tests, because the acceptance host has one render node.
- **Preflight negatives** — one test per diagnostic in §6, including the real
  `No usable encoding profile found` string observed on the stock driver.
- **Output facts** — verify the committed artifact's codec, profile, and pixel format
  match the profile, and that no software fallback occurred.
- **Hardware acceptance** — a `scripts/accept-vaapi-video-acceleration.sh` companion
  to `scripts/accept-nvidia-video-acceleration.sh`, recording assignment evidence and
  verified output facts on the bound device.

Per AGENTS.md, unit tests live in sibling `<source>_test.rs` files linked by
`#[path]`, and no test pairs `tokio::time::pause()` with a real `SqlitePool`.

## 10. Known limits

1. **Single physical device.** Per-device capacity and no-cross-device assignment are
   covered by scheduler unit tests, not a real-media two-device run. ADR 0049's
   two-GPU concurrency evidence has no counterpart on this host.
2. **No session enumeration.** Capacity-probe failures cannot be attributed (§6).
3. **RPM Fusion dependency** for H.264/HEVC encode (§2.1). AV1 encode and all decode
   work on stock Fedora.
4. **`h264_vaapi` and `av1_vaapi` encode are proven but unshipped** (§1). AV1 is
   notable because it needs no third-party driver, so it is the natural next slice
   and would retire ADR 0049 §1's AV1 exclusion for the VAAPI backend.

## 11. Acceptance traceability

Each of issue #409's five acceptance criteria, and where it is discharged. This is the
completion check — the slice is done when every row is satisfied.

| #409 criterion | Discharged by | Falsifiable how |
|---|---|---|
| Real-media encode and supported decode paths execute through the selected render node | §7 command shapes; §9 hardware acceptance script | `scripts/accept-vaapi-video-acceleration.sh` exits 0 and records the bound PCI address |
| Device identity stable across render-node enumeration changes, **or** the limitation explicitly detected and rejected | §4 PCI-address anchor | unit test: a config naming a PCI address with no matching `by-path` entry fails startup; identity survives a `renderD*` renumber because it is not derived from the node number |
| Missing permission, driver, codec, format, or device fails preflight with actionable diagnostics | §6 table | one negative test per row, asserting the distinct message |
| Scheduler tests cover physical-device capacity, compatibility, recovery, no cross-device assignment | §9 scheduler unit tests | four named test groups; single-device host is recorded as a limit in §10, not silently skipped |
| Hardware acceptance verifies assignment evidence and output facts with no implicit software-frame fallback | §9 output-fact tests + acceptance script | committed artifact's codec/profile/pixel format match the profile; no software encoder appears in the recorded argv |

## 12. Guardrails

`just ci` — `fmt-check`, `lint`, `check-test-layout`, `check-paused-time-db`(+selftest),
`check-payload-deny-unknown`(+selftest), `check-adr-index`(+selftest), `test`, `doc`,
`deny`, `audit`. CI runs the umbrella recipe on ubuntu-latest and macos-latest, so
`check-adr-index` hard-gates this PR and ADR 0051's index row ships with it.

Branch: `feat/vaapi-video-acceleration-409`. Base: `main`.
