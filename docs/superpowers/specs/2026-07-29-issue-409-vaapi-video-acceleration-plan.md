# Issue #409 Linux VAAPI Video Acceleration Implementation Plan

## Durable execution facts

- Branch: `feat/vaapi-video-acceleration-409`
- Base: `main`
- Worktree: none — single-session run in the primary working directory
- Assigned ADR: `0051` (index row already committed; renumbered from 0050, which
  issue #414 took on `main` in PR #426)
- Assigned migration: `0030`
- Full guardrail: `just ci`
- Individual guardrails: `just fmt-check`, `just lint`, `just check-test-layout`,
  `just check-paused-time-db`, `just check-payload-deny-unknown`, `just check-adr-index`,
  `just test`, `just doc`, `just deny`, `just audit`
- Hosted gates: Ubuntu `just ci`, macOS `just ci`, coverage/SonarCloud
- Spec: `docs/superpowers/specs/2026-07-29-issue-409-vaapi-video-acceleration-design.md`
- ADR: `docs/adr/0051-vaapi-device-identity-and-probe-proven-capability.md`
- Hardware evidence for command shapes and option ranges is in the spec §2; do not
  re-derive it, and do not invent a command shape the spec does not record.

## Outcome

`hevc_vaapi` encode (Main and Main10) plus typed VAAPI decode for `h264`/`hevc`/`av1`,
dispatched only to a worker bound to a PCI-addressed AMD device whose capability was
proven by a probe encode at startup. Software and NVIDIA behavior byte-for-byte
unchanged. No silent software fallback anywhere.

## Repo conventions that apply to every task

- Unit tests live in a sibling `<source>_test.rs` linked by
  `#[cfg(test)] #[path = "<source>_test.rs"] mod tests;`. `just check-test-layout`
  enforces it. Integration tests stay in `crates/*/tests/`.
- Never pair `tokio::time::pause()`/`advance()` with a real `SqlitePool`; drive DB tests
  on real time and control domain time via the injected `Clock`. `just check-paused-time-db`
  enforces it.
- `[workspace.lints]` denies `unwrap`/`expect`/`panic`; pedantic clippy is on and
  `just lint` runs `-D warnings`.
- Durable JSON payload types carry `#[serde(deny_unknown_fields)]` on the real serde
  unit; tagged enums are not annotated but their variants are newtypes over annotated
  structs. Inline tagged struct-variants are forbidden. `just check-payload-deny-unknown`
  enforces it.
- Error `code` strings are public contract. **This slice adds none** (spec §6).
- `voom` CLI emits exactly one JSON envelope on stdout; logs go to stderr.

---

## Open findings carried between tasks

Found while reviewing completed tasks. Each is verified against the code, names its
owning task, and **must be closed by that task**. Do not treat any of these as
pre-existing noise: every one is reachable the moment a VAAPI profile exists.

| # | Finding | Owner |
|---|---|---|
| F1 | `spawn.rs::video_hardware_requirement` reads `if profile.encoder != "hevc_nvenc" { return software() }`, so a migration-0030 `hevc_vaapi` profile is given a **software** requirement and dispatched to a software worker. This is the silent software fallback issue #409 explicitly forbids. | Task 7 |
| F2 | `repo/execution/workers.rs::accelerator_operation_capacity` groups on `json_extract(extra,'$.accelerator.hardware_token')` and reads `'$.accelerator.max_sessions'` (three queries, lines ~730-768). `VaapiVideoAcceleratorDescriptor` has **no `hardware_token` field** — only the assignment does — so a VAAPI descriptor written to `extra` silently yields no capacity row and the device never receives work. Task 5 must either write a `hardware_token` into the stored extras or change these keys, and must decide whether stored extras stay untagged (`video_hardware.rs` currently parses them as NVIDIA-only). | Task 5 |
| F3 | The VAAPI descriptor carries no `hardware_token`, so the `vaapi:pci-<addr>` token must be derived at the binding site rather than read off the descriptor. | Task 5 |
| F4 | `transcode/events.rs::hardware_evidence` returns `("vaapi", token, None)` for the VAAPI arm — `hardware_device_uuid` absent, which is correct — but the arm has **no test** and no `events_test.rs` sibling exists. | Task 8 |
| F5 | `handler.rs` currently treats a VAAPI assignment as merely "non-software"; `spawn.rs::compatible_assignment` returns `VoomError::Internal` for a VAAPI requirement. Both are deliberate fail-loud placeholders to be replaced, not kept. | Task 6 / Task 7 |
| F6 | `transcode_video_notes` in `planner/transcode_video/mod.rs` emits `cq=0` for a qp-domain profile (`cq.unwrap_or_default()` with `crf`/`cq` both `None`). See Task 7 step 5. | Task 7 |
| F7 | ~~Pre-existing (#400): `worker_capabilities.extra` classified Class P but read typed.~~ **Closed by Task 5** — promoted P→T-upstream with a Class-T row, recording why NVIDIA extras stay untagged and VAAPI's are tagged. | done |
| F8 | **Regression on a mixed host, created deliberately by Task 5 (`2c13aa31`) and left for Task 7.** `video_hardware.rs::candidate_accelerator_descriptor` still returns the NVIDIA struct, and is called per candidate from `spawn.rs` (3 sites) plus `tool_preflight.rs`. A live VAAPI worker now makes it return `Err(Config)`, which **poisons `transcode_video` candidate projection for software and NVIDIA jobs too** — ADR 0049 §6 forbids an error escaping projection. Task 5 chose a loud error over `Ok(None)` because `Ok(None)` would let a VAAPI worker satisfy software preflight (the forbidden fallback), and only corrected the message so a well-formed VAAPI descriptor is no longer called "malformed NVIDIA". **Task 7 must retype it to the tagged `VideoAcceleratorDescriptor` and update all four call sites.** Until then, a host running both a VAAPI worker and any other worker cannot project `transcode_video` candidates. | Task 7 |
| F9 | **Main10 is never probed.** ADR 0051 says one encode per candidate codec, and the descriptor has nowhere to record "Main10 proven", so probing it could only be a hard startup requirement — which would refuse an 8-bit-only device, something the spec does not sanction. The probe is nv12/8-bit only. Issue #409 requires Main **and** Main10, so Main10 rests on the Task 8 acceptance script and hand verification, not on a startup guarantee. Record this limitation in the PR body; do not silently claim Main10 is probe-proven. | Task 8 (verify), Task 9 (disclose) |
| F11 | **VAAPI cannot downscale — a real capability gap, not a bug.** Task 6 (`1f386652`) makes `video_filter_args` return an error when a VAAPI profile sets `max_width`/`max_height` and the source exceeds the cap (`vaapi_refuses_a_downscale_it_has_no_verified_filter_for`). This is the correct call: spec §7 records no `scale_vaapi` shape, so downscaling would mean inventing an unverified command, and silently ignoring the cap would emit output violating the profile. But `max_width`/`max_height` are ordinary policy fields (`voom-policy/src/data/video_profile.rs:28,30`), so **any policy pairing a dimension cap with a VAAPI profile fails per-file on oversized sources** — while the equivalent software profile downscales. Disclose in the PR body and the Task 8 runbook: a VAAPI profile must either omit the caps or be paired with sources already within them. A verified `scale_vaapi` shape is follow-up work, not this slice. | Task 8 (document), Task 9 (disclose) |
| F17 | **The worker still dispatches on encoder *name strings*, so a future backend silently falls into the software arm.** Three wildcard arms introduced on this branch, all matching `profile.encoder.as_str()`: `ffmpeg.rs:602` (`_ => Ok(())` — a new hardware encoder would get **no** input args, so no `-vaapi_device` / `-hwaccel`), `ffmpeg.rs:689` (`_ => Ok(scale_args(..))` — it silently gets the **software** scale filter), and `handler.rs:738` (`_ => validate_software_binding(..)` — it is validated as **software** and accepted on an unbound worker). Because the match subject is a `&str`, the compiler *requires* the wildcard — it cannot warn. **Not a live bug:** all five current encoders are handled explicitly and the software ones belong in that arm. But this is F1's exact failure mode one layer down, and Task 7 fixed the same pattern in the control plane by dispatching on `encoder_descriptor(&profile.encoder).backend`, so the two layers now use different idioms for one decision. **Recommendation: convert all three to match on `VideoEncoderBackend`,** making them exhaustive so a fifth backend fails to compile instead of quietly becoming software work. Small, mechanical, and directly prevents a recurrence of the bug this slice already had. Task 9's step 5 is exactly this check — decide fix-now vs follow-up issue. | Task 9 |
| F16 | **The surface-vs-file pixel-format confusion appeared in FIVE places, four of them fatal to the VAAPI happy path.** A VAAPI profile's `pixel_format` is a GPU *surface* format (`nv12`/`p010`); the encoded file reports `yuv420p`/`yuv420p10le`. Comparing the two is always false for a conforming encode. Found and fixed across Tasks 6-7: `probe_output` (Task 6); `planner::pixel_format_needs_change` — **a conforming output was replanned for transcode forever, so compliance never converged**; `dispatch::validate_output_facts` — **every conforming VAAPI result rejected as malformed, which alone made the backend unusable with device, argv and scheduling all correct**; `resolve::decide_copy_video` — a `copy_compatible` profile silently re-encoded; `handler::validate_copy_pixel_format` (F13). Resolved by moving the measured §2.2 pairings onto `EncoderDescriptor::surface_output_pixel_formats` (empty for software/NVENC, where `pixel_formats` are already file formats and the mapping is the identity) behind one accessor, `voom_core::expected_output_pixel_format`, which all five sites now call — including the worker's, which delegates rather than keeping a second table. `every_declared_pixel_format_has_a_recorded_output_format` asserts the mapping is **total** over every descriptor, so adding a surface without recording what it writes fails loudly instead of silently turning a conforming encode into a hard failure. **Lesson for review: every one of these passed unit tests before the fix, because no unit test compares a requested surface against a real encoded file.** | resolved — disclose in PR body |
| F15 | **Pre-existing spec/code disagreement, corrected in the spec not the code.** Spec §6 (inherited from ADR 0049 §5) said a recognized codec absent from every live descriptor becomes a ticket-scoped `MissingCapability`; the code records `NoEligibleWorker`. The class exists with its own `ErrorCode`, but `cases/execution/tickets.rs::pre_lease_failure_reason` accepts only `NoEligibleWorker` and `AmbiguousWorkerSelection`, and ADR 0049 §10 assigns `NO_ELIGIBLE_WORKER` to this case. #400 shipped that way, so it predates #409. Task 7 correctly implemented VAAPI identically to NVIDIA; widening the path would change NVIDIA's observable failure class. Spec §6 now carries a correction note; ADR 0049 is left unedited (an ADR records a decision, not current code). **Aligning both backends is a follow-up issue, not this slice.** | resolved — disclose in PR body, file follow-up |
| F12 | **Hardware token formatted in three places.** Task 6 added `voom_worker_protocol::vaapi_hardware_token()`, but `local_worker.rs` still formats `vaapi:pci-{}` inline at ~79 and ~708. If the scheduler's token ever disagrees with the worker's, dispatch silently stops matching — the capacity SQL groups on exactly that string (`json_extract(hardware,'$[0]')`). Adopt the helper at both sites and pin scheduler-token == worker-token for the same PCI address. | Task 7 |
| F13 | **Open decision: may a VAAPI profile be `copy_compatible`?** `handler.rs::validate_copy_pixel_format` compares the source's *file* format (`yuv420p`) against the profile's *surface* format (`nv12`), so `copy_compatible: true` on a VAAPI profile always fails at the worker. `-c:v copy` runs no encoder, so a stream copy is not a hardware operation and arguably should succeed. Same surface-vs-file category error Task 6 fixed in `probe_output` (which would otherwise have failed *every* conforming VAAPI encode). Either reject `copy_compatible` on VAAPI profiles at policy-compile time, or compare against `expected_output_pixel_format`. Do not leave a profile an operator can author that always fails. | Task 7 |
| F14 | Two timing-sensitive tests flaked once each under heavy parallel load and passed on re-run: `preflight::tests::vaapi_capacity_clock_expiry_names_the_declaration` (Task 5, seen by Task 6) and previously `wait_child_output_times_out_promptly_when_a_grandchild_holds_the_pipe` (fixed via ETXTBSY retry). Clock-expiry tests asserting *prompt* return are inherently load-sensitive. Confirm against the hosted gates; if either flakes in CI, widen the elapsed bound rather than deleting the assertion — the bound is the point of the test. | Task 9 |
| F10 | Two test-environment assumptions to confirm against the hosted gates, which cannot be reproduced locally: diagnostic 4's permission-denied test needs a **non-root** user (`EACCES` is unobservable as root), and the "not a character device" test plus the fake DRI tree need to behave on **macOS**, which is a hosted `just ci` gate. Neither is exercised by the local Linux run. | Task 9 |

---

## Task 1: Add the VAAPI decode variant and make backend predicates explicit

### Fit

Prerequisite for every later task. `VideoDecodeMode::is_nvidia()` is currently
`!self.is_software()`, so a third backend silently makes it report `true` for VAAPI —
and it gates profile validation at `transcode_video_profile.rs:83`.

The variant and the predicate fix land **together**, deliberately. Fixing the predicate
alone has no red state: any test written against the two existing variants passes under
the buggy definition, because with only `Software` and `Nvidia` the negation is
accidentally correct. Adding `Vaapi` is what makes the defect observable, so it is what
makes the test genuinely fail first.

### Files

- `crates/voom-core/src/media/transcode_video_profile.rs`
- `crates/voom-core/src/media/transcode_video_profile_test.rs`

### TDD

1. Add `VideoDecodeMode::Vaapi(VaapiVideoDecode)` with its `deny_unknown_fields`
   content struct, and extend `parse`/`as_str` with `"vaapi"`.
2. Add a now-genuinely-failing test asserting `is_nvidia()` is **false** for
   `VideoDecodeMode::Vaapi`, plus `is_vaapi()` true for it and false for the other two,
   and that `parse("vaapi")` round-trips.
3. Rewrite `is_nvidia()` as an explicit `match` mirroring `is_software()`, and add
   `is_vaapi()` the same way.

### Acceptance

- `is_software()`, `is_nvidia()`, and `is_vaapi()` are all explicit matches; none is
  defined as the negation of another.
- No wildcard `_ =>` arm is introduced (repo style: explicit destructuring catches
  field and variant changes).
- Step 2's test is observed failing before step 3 is written.

### Verify

`just fmt-check && just lint && cargo test -p voom-core`

### Rollback

Self-contained; revert the commit.

---

## Task 2: Typed VAAPI vocabulary in `voom-core`

### Fit

The protocol-neutral capability vocabulary every other layer validates against.
Depends on Task 1.

### Files

- `crates/voom-core/src/media/encoder_caps.rs`
- `crates/voom-core/src/media/encoder_caps_test.rs`
- `crates/voom-core/src/media/transcode_video_profile.rs`
- `crates/voom-core/src/media/transcode_video_profile_test.rs`
- `crates/voom-core/src/lib.rs` (re-exports)

### TDD

1. Failing tests for:
   - `qp` accepted at 1 and 52; rejected at 0 and 53 (spec §2.2 — 0 means auto);
   - a VAAPI profile with `preset: Some(_)` rejected, and `preset: None` accepted;
   - a non-VAAPI profile with `preset: None` rejected;
   - `codec_level` rejected for `hevc_vaapi` (empty `codec_levels`);
   - `codec_profile: "main"` with `pixel_format: "p010"` rejected via
     `eight_bit_only_profiles`;
   - `pixel_format` outside `["nv12","p010"]` rejected;
   - VAAPI decode paired with a non-VAAPI encoder rejected, and vice versa;
   - `crf`/`cq` rejected for `hevc_vaapi`.
2. Add `VideoEncoderBackend::Vaapi`, `QualityDomain::Qp { min, max }`,
   `PresetDomain::None`, the `HEVC_VAAPI` descriptor exactly as tabulated in spec §3,
   and `VAAPI_VIDEO_DECODERS: &[&str] = &["h264","hevc","av1"]` (a flat list — **not**
   the `(codec, decoder)` pair form; see spec §3 for why).
3. Change `TranscodeVideoProfile.preset` to `Option<String>` and add `qp: Option<u8>`.
   (`VideoDecodeMode::Vaapi` already landed in Task 1.)
4. Extend `validate_profile_against_descriptor`: add the `Qp` arm to the quality match
   (keep it exhaustive over `(quality_domain, crf, cq, qp)`), add preset-presence
   validation driven by `PresetDomain`, and add the VAAPI decode/encoder pairing rule
   alongside the existing NVIDIA one.

### Acceptance

- Every existing `voom-core` test still passes with `preset` as `Option`.
- The quality-domain match has no wildcard arm and covers all three domains.
- `PresetDomain::None` requires `preset.is_none()`; every other domain requires `Some`.

### Verify

`just fmt-check && just lint && cargo test -p voom-core && just check-test-layout`

---

## Task 3: Migration 0030 and durable profile storage

### Fit

Persists the Task 2 vocabulary. Must land before any read path expects `qp` or a
nullable `preset`.

### Files

- `migrations/0030_vaapi_video_acceleration.sql`
- `crates/voom-store/src/repo/policy/video_profiles.rs` (+ its `_test.rs`)
- `crates/voom-store/src/schema_test.rs`
- `crates/voom-store/src/migrator.rs` if embedded-migration assertions require it
- `docs/payload-contract-inventory.md`
- `scripts/payload-contract-scope.txt`

### TDD

1. Failing schema tests asserting the new CHECKs reject: `hevc_vaapi` with a non-null
   `preset`; `hevc_vaapi` with `qp` outside 1..52; `hevc_vaapi` with a non-null `crf` or
   `cq`; a non-VAAPI encoder with a null `preset`; `decode_backend = 'vaapi'` with a
   non-`hevc_vaapi` encoder; `decode_backend` outside the three-value vocabulary.
2. Failing repository round-trip tests for a VAAPI profile (null `preset`, set `qp`,
   `decode_backend = 'vaapi'`) and for an unchanged software profile.
3. Write migration 0030 as a table rebuild following
   `0029_nvidia_video_acceleration.sql`'s exact pattern (`video_profiles_new` → INSERT
   projection → DROP → RENAME), per spec §8. Existing rows project straight through with
   `NULL AS qp`.
4. Update the repository to read/write `qp` and a nullable `preset`.
5. Add the new durable typed columns to the payload-contract inventory and scope list.

### Acceptance

- Existing profile rows survive the migration unchanged, with `preset` retained and
  `qp` null — assert this with a test that seeds pre-migration rows.
- `STRICT` is preserved on the rebuilt table.
- `just check-payload-deny-unknown` passes.

### Verify

`just fmt-check && just lint && cargo test -p voom-store && just check-payload-deny-unknown`

### Rollback

Migrations are forward-only. If this task is abandoned, drop the migration file before
any DB has applied it; do not add a down-migration.

---

## Task 4: Worker-protocol VAAPI contracts

### Fit

The wire contract between the control plane and FFmpeg workers. Depends on Task 2.

### Files

- `crates/voom-worker-protocol/src/video_acceleration.rs` (+ its `_test.rs`)
- `crates/voom-worker-protocol/src/operations/transcode_video.rs` (+ its `_test.rs`)
- `crates/voom-worker-protocol/src/lib.rs`

### TDD

1. Failing serde tests: a `vaapi`-tagged requirement/assignment round-trips; the
   `backend` tag serializes as `"vaapi"`; an unknown field is rejected; an unknown
   `backend` value is rejected; existing `software`/`nvidia` JSON is unchanged
   byte-for-byte.
2. Add `VaapiVideoAcceleratorDescriptor` (backend, pci_address, device_name,
   driver_version, encoders, decoders, max_sessions), `VaapiVideoHardwareRequirement`,
   and `VaapiVideoHardwareAssignment`, each `#[serde(deny_unknown_fields)]`.
3. Add `Vaapi` variants to `VideoHardwareRequirement` and `VideoHardwareAssignment`.
4. Replace `LocalWorkerBound.accelerator: Option<NvidiaVideoAcceleratorDescriptor>` with
   `Option<VideoAcceleratorDescriptor>`, a new tagged enum over the NVIDIA and VAAPI
   descriptor structs. This is **not** additive — note it in the commit message as a
   coordinated binary-before-DB change per ADR 0013.

### Acceptance

- Tagged enums carry no `deny_unknown_fields`; their variants are newtypes over
  annotated content structs (payload-contract rule).
- Existing NVIDIA descriptor JSON still deserializes.

### Verify

`just fmt-check && just lint && cargo test -p voom-worker-protocol && just check-payload-deny-unknown`

### Rollback

**The riskiest task to revert.** `LocalWorkerBound.accelerator` changes shape, so a
control plane and a worker binary on opposite sides of this commit cannot exchange a
bound-worker payload. Revert Task 4 only together with Tasks 5–7, and never deploy a
mixed pair; ADR 0013's binary-before-DB ordering applies.

---

## Task 5: Worker device binding and probe-proven capability

### Fit

The heart of ADR 0051: resolve a PCI address to a render node, verify it, and prove
capability by executing encodes. Depends on Task 4.

### Files

- `crates/voom-ffmpeg-worker/src/preflight.rs` (+ its `_test.rs`)
- `crates/voom-ffmpeg-worker/src/main.rs` (+ its `_test.rs`)
- `crates/voom-control-plane/src/local_worker.rs` (+ its `_test.rs`)
- `crates/voom-cli/src/cli.rs` and `crates/voom-cli/src/commands/execution/worker.rs`
- new decoder probe fixtures under `crates/voom-ffmpeg-worker/tests/fixtures/`

### Test seams — establish these first

The NVIDIA slice makes probes testable without a GPU by **injecting binary paths and
device identity through env vars**, not through a trait: `VOOM_NVIDIA_SMI_BIN`,
`VOOM_FFMPEG_BIN`, `VOOM_FFPROBE_BIN`, `VOOM_NVIDIA_DEVICE`,
`VOOM_NVIDIA_MAX_SESSIONS` (`crates/voom-ffmpeg-worker/src/preflight.rs:14-16,86-87`;
`crates/voom-control-plane/src/local_worker.rs:502-503`). Tests point the binary vars at
fake scripts. Follow that pattern rather than introducing an abstraction:

- `VOOM_VAAPI_DEVICE` — the configured PCI address, mirroring `VOOM_NVIDIA_DEVICE`.
- `VOOM_VAAPI_MAX_SESSIONS` — declared capacity, mirroring `VOOM_NVIDIA_MAX_SESSIONS`.
- `VOOM_FFMPEG_BIN` — **already exists**; reuse it to fake both probe encodes and the
  decoder probe. No new binary seam is needed, because VAAPI needs no `nvidia-smi`
  analogue: identity comes from the filesystem, not an external tool.
- **One genuinely new seam:** a DRI root override (e.g. `VOOM_DRI_ROOT`, defaulting to
  `/dev/dri`) so a test can build a fake `by-path/pci-<addr>-render` tree in a tempdir
  and exercise resolution, readback mismatch, absent-node, and permission-denied without
  touching the real `/dev/dri`. Without this, spec §6's first five diagnostics are not
  testable on a machine whose real device disagrees with the fixture.

Add the seams before the diagnostics tests; they are the prerequisite that makes the
tests writable.

### TDD

1. Failing tests for each spec §6 diagnostic: malformed PCI address; unresolvable
   `by-path`; absent render node; permission denied; readback mismatch; codec absent
   from the driver build; probe-encode failure; capacity-probe failure; clock expiry.
   Each asserts a distinct, actionable message. Drive the first five through the DRI
   root override and the rest through a fake `VOOM_FFMPEG_BIN`.
2. Implement PCI-address config parsing and `by-path` resolution, then the readback
   comparison (spec §4). Reject a render-node path or ordinal in config.
3. Implement the encoder probe (synthesize with `lavfi testsrc`, upload, encode, require
   non-empty output) and the decoder probe (decode a bundled 4:2:0 fixture with
   `-hwaccel_output_format vaapi`, require clean exit). Generate fixtures with an
   explicit `-pix_fmt yuv420p` — spec §2.3 explains why the obvious
   `testsrc`-to-`libx265` recipe yields undecodable `gbrp`.
4. Implement the concurrent capacity probe bounded `1..=16`, defaulting to 1, and wire
   ADR 0051 §7's clocks (per-probe timeout, one-minute capacity clock, five-minute
   readiness deadline). Reuse ADR 0049's existing clock plumbing rather than adding new
   configuration.
5. Advertise the `vaapi:pci-<addr>` hardware token and the typed descriptor in `extra`.
   Never cache a probe result across restarts.

### Acceptance

- No code path advertises a codec that has not encoded on the bound device in this
  process.
- A capacity-probe failure reports diagnostic uncertainty and never attributes the cause
  to external contention (ADR 0051 §6 — VAAPI has no session enumeration).
- `run-local`'s two-line stdout contract is unchanged.
- Tests do not require a GPU: every probe and device lookup routes through the seams
  above, and the real-hardware path is exercised by Task 8's acceptance script.

### Verify

`just fmt-check && just lint && cargo test -p voom-ffmpeg-worker -p voom-control-plane && just check-paused-time-db`

### Rollback

The new env vars are additive and default to the previous behavior (no VAAPI device
configured → no VAAPI descriptor advertised), so reverting this task leaves software and
NVIDIA workers untouched.

---

## Task 6: Deterministic FFmpeg command generation

### Fit

Turns a validated VAAPI profile into the exact argv the spec pins. Depends on Tasks 2
and 4.

### Files

- `crates/voom-ffmpeg-worker/src/ffmpeg.rs` (+ its `_test.rs`)
- `crates/voom-ffmpeg-worker/src/handler.rs` (+ its `_test.rs`)
- `crates/voom-ffmpeg-worker/tests/transcode_conformance.rs`
- `crates/voom-cli/tests/snapshots/` if envelope snapshots shift

### TDD

1. Failing `insta` conformance snapshots pinning argv for all three spec §7 shapes,
   8-bit and 10-bit:
   - software decode → hw encode: `-vaapi_device <node> … -vf format=nv12,hwupload -c:v hevc_vaapi -rc_mode CQP -qp N`;
   - hw decode → hw encode: `-hwaccel vaapi -hwaccel_device <node> -hwaccel_output_format vaapi -i … -c:v hevc_vaapi …` with **no** `-vf`;
   - Main10: `format=p010`, plus `-profile:v main10` only when `codec_profile` is set.
2. Assert `-rc_mode CQP` is always present and `auto` never relied on.
3. Assert `codec_profile` is passed through **by name** (`-profile:v main10`), with no
   name-to-integer mapping — `hevc_vaapi`'s `-profile` carries named constants (spec §2.2).
4. Assert `-level` is never emitted.
5. Assert no software encoder ever appears in a VAAPI command, on any error path.

### Acceptance

- Snapshots are reviewed with `cargo insta review` and committed deliberately.
- Existing software and NVENC snapshots are untouched.

### Verify

`just fmt-check && just lint && cargo test -p voom-ffmpeg-worker`

Do **not** run `just test` here — it builds the whole workspace with `--all-features`
twice and is deferred to Task 9, which is the first point AGENTS.md expects it.

---

## Task 7: Policy validation, planning, preflight, and scheduling

### Fit

Makes a VAAPI profile selectable, requirement-derived, and dispatchable only to a
compatible device. Depends on Tasks 2–5.

### Files

- `crates/voom-policy/src/compile/validate/operations.rs` (+ `validate_test.rs`)
- `crates/voom-policy/src/compile/lower/operations.rs`
- `crates/voom-policy/src/data/video_profile.rs` (+ its `_test.rs`)
- `crates/voom-plan/src/planner/transcode_video/profile.rs` (+ its `_test.rs`)
- `crates/voom-plan/src/planner/transcode_video/mod.rs` (+ `planner_test.rs`) — carries
  the open finding below
- `crates/voom-control-plane/src/cases/policy/tool_preflight.rs` (+ its `_test.rs`)
- `crates/voom-control-plane/src/transcode/{dispatch,resolve,mod}.rs` (+ their `_test.rs`)
- `crates/voom-control-plane/src/cases/execution/leases_test.rs`
- `crates/voom-control-plane/src/workflow/execution/executor/{mod,spawn,config}.rs`

### TDD

1. Failing policy-DSL tests: a `decode: vaapi` clause accepted with `hevc_vaapi` and
   rejected with any other encoder; `qp` accepted in range; `preset` rejected for VAAPI.
   Note `validate_test.rs:139` already contains a `decode: vaapi` case written against
   the NVIDIA slice — reconcile it rather than duplicating it.
2. Failing preflight tests: a VAAPI transcode requires an identity-verified
   `hevc_vaapi` descriptor; a `vaapi`-decode profile additionally requires ≥1 usable
   VAAPI decoder; an unsupported source codec becomes a planner-blocked file; a
   recognized codec absent from every live descriptor becomes a ticket-scoped
   `MissingCapability`.
3. Failing scheduler tests: compatible-device selection; per-device capacity exhaustion;
   deterministic worker-ID tie-breaking; claim recovery after proven owner death; and no
   cross-device assignment. These are unit tests — the acceptance host has one render
   node (spec §10), and that limit is recorded, not skipped.
4. Assert dispatch repeats endpoint identity validation before acquiring the lease.
5. **Open finding carried from Task 2 (review of `a983c196`) — must be fixed here.**
   `transcode_video_notes` in `planner/transcode_video/mod.rs` derives its quality note
   as `crf` else `cq`, with `cq.unwrap_or_default()`. A VAAPI profile has `crf: None`,
   `cq: None`, `qp: Some(n)`, so it emits **`cq=0`** — a false statement about the
   profile in operator-facing plan and compliance-report output. Task 2 correctly made
   the *preset* note conditional in this same function but left the quality note with no
   `qp` branch. Write a failing test asserting a qp-domain profile's notes contain
   `qp=<n>` and no `cq=`, then make the quality note exhaustive over the three domains.
   Do not reintroduce `unwrap_or_default()` as a stand-in for an absent value.

### Acceptance

- A software profile still requires an unaccelerated worker; an NVIDIA profile still
  requires an NVENC descriptor. Neither path changes behavior.
- A GPU-bound VAAPI worker does not execute software video profiles (ADR 0049 §5 rule,
  applied to the new backend).

### Verify

`just fmt-check && just lint && cargo test -p voom-policy -p voom-plan -p voom-control-plane`

---

## Task 8: CLI surface, event evidence, acceptance script, and docs

### Fit

Operator-facing completion and the durable assignment evidence issue #409 requires.
Depends on Tasks 3–7.

### Files

- `crates/voom-cli/src/commands/media/profile.rs` (+ its `_test.rs`)
- `crates/voom-cli/tests/profile_envelope.rs`
- `crates/voom-events/src/payload/artifact.rs` (+ its `_test.rs`)
- `crates/voom-fake-support/src/results.rs`
- `crates/voom-control-plane/src/.../report_previews_combined_multi_phase_policy.snap`
  if adding a VAAPI profile shifts report previews — #400 had to update this snapshot,
  so expect it rather than being surprised by a red `just test` in Task 9
- `scripts/accept-vaapi-video-acceleration.sh`
- `docs/runbooks/operator-real-media-execution.md`
- `justfile` if a new recipe is warranted

### TDD

1. Failing CLI envelope tests for creating and reading a VAAPI profile (`qp` present,
   `preset` absent, `decode: vaapi`), asserting the single-envelope stdout contract.
2. Failing event tests asserting `hardware_backend` records `"vaapi"` on a VAAPI
   transcode artifact and stays `None` for software.
3. Write `scripts/accept-vaapi-video-acceleration.sh` modelled on
   `scripts/accept-nvidia-video-acceleration.sh`: bind the PCI address, run a real-media
   encode, and record the bound device, the argv, and the verified output facts
   (codec/profile/pixel format). It must fail if a software encoder appears in the argv.
4. Document the operator setup in the runbook, including the RPM Fusion
   `mesa-va-drivers-freeworld` dependency for HEVC (spec §2.1) and the fact that AV1
   encode and all decode work on stock Mesa.

### Acceptance

- Every row of spec §11's acceptance-traceability table is satisfied and demonstrably
  checkable.
- The runbook states the driver dependency plainly rather than implying stock Fedora
  suffices for HEVC.

### Verify

`just ci` — the full suite, then `./scripts/accept-vaapi-video-acceleration.sh` on the
acceptance host recorded in spec §2.

---

## Task 9: Full verification

### Work

1. `just ci` green locally (this is the first point the full `--all-features` test build
   and `just doc` run; both are CI-only per AGENTS.md).
2. `cargo insta review` for any deliberate snapshot change; no accepted snapshot may
   contain a software encoder in a VAAPI command.
3. Confirm no new `VoomError` code string was added (spec §6).
4. Confirm software and NVIDIA command shapes are byte-for-byte unchanged by diffing
   their conformance snapshots against `main`.
5. Re-read the diff for `unwrap`/`expect`/`panic` introductions and wildcard match arms.

### Acceptance

`just ci` exits 0 with no warnings, and the acceptance script's recorded evidence is
attached to the PR.
