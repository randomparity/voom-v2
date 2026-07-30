---
status: accepted
date: 2026-07-29
deciders: [VOOM core]
---

# 0052 — VAAPI device identity is the PCI address, and capability is probe-proven

Issue: #409

## Context

ADR 0049 established the typed accelerator-resource model and proved it with
NVIDIA, deferring VAAPI so its command shapes and failure behavior could be
tested on matching hardware. That hardware is now available: an AMD Strix Halo
APU (Radeon 8060S) on Fedora 44, Mesa 26.1.5 `radeonsi`, FFmpeg 8.1.2.

VAAPI does not supply the primitives ADR 0049 leaned on for NVIDIA. It has no
device UUID, no `nvidia-smi` equivalent for identity readback, and no encoder-session
enumeration. Two further properties fall out of the acceptance probe:

1. **Usable codecs are a property of the loaded driver build, and are invisible
   from the device.** On the acceptance host, stock `mesa-dri-drivers` advertises
   AV1 encode only, while `mesa-va-drivers-freeworld` on the same GPU, same render
   node, and same FFmpeg additionally advertises H.264 and HEVC encode. Installing
   the freeworld package places `/usr/lib64/dri-freeworld/` on the global library
   path and makes it the system-wide default, so the swap is not visible in the
   render-node path, in FFmpeg's encoder list, or in any per-process configuration.
   The same `vainfo` binary reports different capability depending only on which
   driver build got loaded.

2. **Render-node numbers are not identity.** `/dev/dri/renderD128` is assigned by
   enumeration order and can renumber; the PCI address behind it cannot.

The design detail is recorded in the [VAAPI video acceleration design][vaapi-design].

[vaapi-design]: ../superpowers/specs/2026-07-29-issue-409-vaapi-video-acceleration-design.md

## Decision

1. **A VAAPI worker is configured with a PCI address, never a render-node path or
   ordinal.** Startup resolves the address through
   `/dev/dri/by-path/pci-<addr>-render`, reads the resolved node's PCI address back,
   and fails startup on a mismatch. The hardware token is `vaapi:pci-<addr>`. This
   shares ADR 0049 §2's UUID-not-ordinal *motive* and satisfies issue #409's
   stability requirement rather than its detect-and-reject fallback.

   The readback is a weaker check than its NVIDIA counterpart, and deliberately so.
   ADR 0049 needs a PID-to-UUID readback because `CUDA_VISIBLE_DEVICES` plus "CUDA
   device zero" is an indirection that can land FFmpeg on a device the scheduler did
   not choose. VAAPI has no such indirection: `-vaapi_device <node>` opens exactly
   that node, so binding strength comes from naming the device directly at open time,
   not from the readback. The readback's job is narrower — catching a stale or
   incorrect `by-path` symlink, since udev generates that symlink from the PCI address
   the check re-reads. It is not, and is not relied on as, proof that the encode ran
   on the intended device; §2's probe encode establishes that.

   **The token is host-scoped, and that is a precondition rather than a property.**
   ADR 0049's `nvidia:GPU-<uuid>` is globally unique and ADR 0051's
   `videotoolbox:<hash>` identifies a machine by construction; a PCI address is
   unique only within one machine, and `0000:03:00.0` is an ordinary slot. So
   `vaapi:pci-<addr>` assumes **one Linux host per control plane**. Two hosts sharing
   one would pool the capacity of two physically distinct devices under a single
   token, and the boot-id claim recovery in §5 would let either host read the other's
   live claim as abandoned. Lifting the assumption means qualifying the token with a
   boot-invariant host identity such as `/etc/machine-id`, which changes every stored
   token and so belongs to whichever change first needs multi-host accelerators — not
   to this one, which has no such requirement. Recorded here, and on
   `vaapi_hardware_token`, so it is checkable rather than implicit.

2. **Advertised capability is proven by executing a claim-owned smoke encode on the
   bound device, per codec.** FFmpeg's encoder list and `vainfo`'s entrypoint list
   are necessary but never sufficient, because neither distinguishes the two driver
   builds above. A codec that has not encoded on this device in this process's
   driver environment is not advertised.

3. **The first slice is `hevc_vaapi` encode — Main and Main10 — plus typed VAAPI
   decode for `h264`, `hevc`, and `av1`.** `h264_vaapi` and `av1_vaapi` encode are
   proven on the acceptance host but deferred to follow-up issues, keeping this
   slice's conformance surface to the codec that matches VOOM's existing
   `default-hevc` profile.

4. **VAAPI profiles carry a quality-parameter domain and no preset.** `qp` is
   constrained to `1..=52`: FFmpeg accepts `0..52` and rejects 53, and 0 is the
   default meaning auto, so it is excluded from the operator vocabulary. `hevc_vaapi`
   exposes no `-preset` and no `-compression_level`, so `preset` becomes nullable and
   a migration `CHECK` forbids it for VAAPI while keeping it mandatory for every
   other encoder. `codec_level` is not offered for VAAPI in this slice: FFmpeg derives
   a correct `general_level_idc` automatically, no operator has asked for an explicit
   level, and supporting it would need a normalization step plus per-level
   verification (below). Deferring it is a scope choice, not a limitation of VAAPI.

5. **Rate control is always explicit.** Generated commands pass `-rc_mode CQP -qp N`
   rather than relying on `auto`, so rate-control behavior cannot drift with an
   FFmpeg or driver default. Frame transfers are explicit in both directions:
   `format=nv12,hwupload` (or `format=p010` for Main10) for a software-decoded
   source, and a bare hardware-frame path with no filter for VAAPI-decoded input.
   There is no software-encoder fallback.

6. **Declared concurrency is operator-supplied, bounded `1..=16`, and proven by
   concurrent smoke encodes.** The bound and its rationale are ADR 0049 §3's, adopted
   unchanged: it stops startup creating unbounded probe processes. VAAPI exposes no
   session enumeration, so ADR 0049 §3's separation of external contention from a VOOM
   orphan has no VAAPI counterpart, and a failed capacity probe is always reported as
   diagnostic uncertainty. No VA-API version specifies such a query today, so this
   slice treats the absence as settled rather than as work in progress.

7. **Probing is bounded by the same clocks as ADR 0049 §3.** Each probe encode carries
   an individual timeout, the concurrent capacity probe reuses the one-minute capacity
   clock, and overall readiness reuses the five-minute deadline; expiry fails startup
   with the codec or capacity that did not prove, rather than leaving the worker
   pending. Without this a hung probe would block readiness indefinitely, and §2
   requires the probe to run on every start.

## Consequences

- Existing software and NVIDIA profile JSON and FFmpeg arguments remain
  byte-for-byte stable. Profile storage gains a nullable `qp`, a widened
  `decode_backend` vocabulary, and a `preset` column that is nullable at the schema
  level but still mandatory per-encoder via `CHECK`.
- H.264 and HEVC VAAPI encode require a driver build carrying those codecs, which on
  Fedora means the RPM Fusion `mesa-va-drivers-freeworld` package. AV1 encode and all
  hardware decode work on the stock driver. Operators who cannot install that package
  get an actionable preflight failure rather than silent software encoding.
- Because capability tracks the loaded driver build rather than the hardware, a host
  driver change can move a worker's advertised codecs without any VOOM configuration
  change. The startup probe is the only thing that detects this, so it runs on every
  start and is not cached across restarts.
- That uncached probe is a per-start cost paid on the GPU: one encode per candidate
  codec, plus as many concurrent encodes as the declared capacity, so a worker
  declaring 16 runs 16 at once before reporting ready. Worker startup is therefore
  measurably slower than a software worker's and consumes device capacity while it
  runs. §7's clocks bound the cost; they do not remove it. Operators restarting many
  workers at once should expect contention during the probe window.
- A failed VAAPI capacity probe cannot attribute the cause. Operators diagnosing
  contention on a shared GPU get less signal than on NVIDIA, by construction.
- `LocalWorkerBound.accelerator` becomes a tagged enum rather than an
  NVIDIA-specific optional struct, so the worker-protocol change is not additive and
  is coordinated binary-before-DB per ADR 0013.
- The acceptance host has a single render node, so per-device capacity and
  no-cross-device assignment are covered by scheduler unit tests rather than a
  real-media two-device run. ADR 0049's two-GPU concurrency evidence has no
  counterpart here.
- `VideoDecodeMode::is_nvidia()` stops being defined as "not software". Adding a
  third backend makes that definition wrong, and it gates profile validation.

## Considered and rejected alternatives

- **Trust `vainfo` entrypoints, or FFmpeg's encoder list, as proof of support.**
  Rejected because both report HEVC encode as available or unavailable on the same
  GPU depending only on which driver build is loaded, and the loaded build is not
  visible from either.
- **Configure the worker with a render-node path.** Rejected because render-node
  numbers are enumeration-order artifacts. The PCI address is the stable anchor and
  resolves to exactly one node.
- **Pin the driver build per worker with `LIBVA_DRIVERS_PATH`.** Rejected as a
  capability guarantee: the acceptance probe showed the freeworld package becomes the
  global default with the variable unset, so a worker that merely sets a path has
  still not established what that path's driver can do. Executing a probe encode is
  the only falsifiable check. The variable remains useful to operators for pinning,
  and is out of scope as a correctness mechanism.
- **Give VAAPI a synthetic one-value preset so `preset` stays `NOT NULL`.** Rejected
  because it puts an operator-facing knob in the vocabulary that maps to no FFmpeg
  flag.
- **Map the profile's preset onto `-async_depth`.** Rejected because `async_depth` is
  processing parallelism, not a speed/quality tradeoff, and it would overlap the
  per-device capacity model in ADR 0049 §3.
- **Do nothing — defer VAAPI until a host with a vendor-supported driver stack and
  more than one device exists.** Rejected, but it is the closest call in this record.
  It would buy a stronger acceptance story: no RPM Fusion dependency, and a real
  two-device capacity demonstration instead of the scheduler unit tests Consequences
  records.
  It loses more than it buys. The acceptance host proves every command shape and every
  failure path this slice ships, and the device model it exercises is the one a later
  Intel or multi-device AMD host would reuse — so deferring postpones working
  acceleration on hardware in hand to obtain test-topology coverage that scheduler
  unit tests already provide. The residual is recorded in Consequences rather than
  hidden: this slice does not demonstrate real-media cross-device assignment, and
  H.264/HEVC encode carries a third-party driver dependency.
- **Include `h264_vaapi` and `av1_vaapi` encode in this slice.** Rejected as scope:
  all three encoders are proven, but each additional encoder multiplies the pinned
  command shapes and preflight permutations without proving anything new about the
  VAAPI device model. They are follow-up issues with hardware already in hand.
- **Ship AV1 as the first slice instead of HEVC.** Rejected, though on a genuinely
  finer margin than the entry above, which argues only against shipping all three.
  AV1 is the only proven encoder needing no third-party driver, so an AV1-first slice
  would carry no RPM Fusion dependency at all — a real advantage this record does not
  dispute. HEVC wins on two grounds. It is the codec VOOM's existing `default-hevc`
  profile and every current software profile target, so it exercises the
  software-to-hardware substitution operators will actually make first; and it is the
  codec ADR 0049 proved for NVIDIA, so shipping it second makes the two backends
  directly comparable on one codec rather than leaving each backend proven on a
  different one. AV1 remains the natural next slice and needs no new hardware.
- **Expose `codec_level` for VAAPI.** Rejected as scope, on an accurate reading of
  what it would cost. `hevc_vaapi`'s `-level` is an int-typed AVOption but carries
  named constants (`1`, `2`, `2.1`, `3`, `3.1`, `4`, `4.1`, `5`, `5.1`, `5.2`, `6`,
  `6.1`, `6.2`), so no name-to-integer table is needed — an earlier draft of this
  record claimed otherwise and was wrong. What it does need is normalizing VOOM's
  existing level vocabulary, which spells the whole levels `4.0`/`5.0`/`6.0` where
  FFmpeg spells them `4`/`5`/`6`, plus a verified encode per level. FFmpeg's
  auto-derivation is correct and no operator has asked for an explicit level, so the
  work buys nothing this slice needs.
- **Expose `rc_mode` as a profile field.** Rejected as speculative. CQP is the mode
  this slice proves; other modes can be added when an operator needs one.
- **Reuse the NVIDIA `max_sessions` probe strategy for capacity.** Rejected because
  VAAPI has no session-count query at all, so the declaration cannot be
  cross-checked against an authoritative number the way NVML permits.
- **Treat the absent session enumeration as a temporary gap, pending a future
  VA-API query.** Rejected because no VA-API version specifies one, so designing
  around an anticipated query would be speculative. Recording the uncertainty as
  settled stops a later issue from chasing a capability that does not exist; should
  VA-API ever add such a query, that is a new decision superseding this one, not a
  gap this record left open.
