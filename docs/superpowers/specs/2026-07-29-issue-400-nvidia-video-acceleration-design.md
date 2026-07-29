# NVIDIA video acceleration

Issue: #400

## Goal

Allow an operator to choose typed HEVC NVENC profiles and dispatch them only to
a local FFmpeg worker bound to a compatible NVIDIA GPU with tested spare
session capacity. Support software decode plus NVENC and NVIDIA decode plus
NVENC without changing any existing software profile or FFmpeg command.

## Success criteria

- Existing software profile JSON, validation, plans, CLI output fields, and
  FFmpeg argument sequences remain unchanged.
- HEVC NVENC profiles use constant-quality (`cq`) settings and a closed
  encoder-specific vocabulary; `crf`, arbitrary options, and mixed quality
  modes are rejected.
- NVIDIA decode is explicit and never selected as a fallback.
- Run-local records exact-device capabilities only after build, driver,
  permission, encoder, decoder, CUDA identity, and configured-capacity probes
  pass.
- Policy preflight rejects a run unless every derived software or
  backend/encoder requirement has a live, identity-verified provider.
- Scheduler selection requires the exact backend, encoder, and decoder and uses
  deterministic tie breaking.
- UUID-wide capacity is checked both for candidates and atomically during lease
  acquisition.
- Worker results, events, and reports identify the assigned GPU.
- Unit/conformance tests cover malformed shapes, command arguments, result
  mismatches, and scheduler rejection reasons.
- Real-media tests on the RTX A6000 and Quadro RTX 4000 run one concurrent HEVC
  job per GPU, read back each FFmpeg PID's GPU UUID, and prove distinct
  assignments and valid output facts.
- AV1 hardware decode is advertised only on devices whose `av1_cuvid` smoke
  probe succeeds. AV1 NVENC is excluded until it can receive real-media
  acceptance on encode-capable hardware.
- `just ci` and accelerator acceptance commands complete without warnings.

## Review dispositions

### Initial review

1. **Device identity:** accepted. Configuration and durable evidence use the
   full GPU UUID. `CUDA_VISIBLE_DEVICES` exposes only that UUID, commands use
   CUDA device zero, and startup reads the FFmpeg PID back through
   `nvidia-smi`. NVML ordinals and `-gpu` are not part of the contract. The
   PID-to-UUID assertion remains falsifiable even though this host's NVML and
   CUDA default orders currently agree.
2. **Conflicting capacity:** accepted. A durable device claim prevents the
   supported local path from creating two owners. Preflight blocks existing
   conflicts before a job; a conflict discovered during a run is a typed
   `MissingCapability` ticket rejection, never a job-fatal projection error.
3. **Temporarily absent accelerator:** accepted. A previously advertised device
   defers without ticket mutation or attempt consumption while candidates and
   runtimes refresh. Timeout fails the job with the ticket still ready.
   Never-advertised hardware retains the existing attempt-consuming backstop
   after a preflight bypass.
4. **Probe isolation:** accepted. The supervisor owns a durable UUID claim and
   the concurrency probes run only in that claim's process group. No probe
   advertises a partial capability.
5. **Eligibility preflight:** accepted. The existing pre-job tool preflight
   derives software and backend/encoder requirements from resolved profiles,
   including the GPU-only-worker/software-profile case.

### Recovery review

1. **Recovery budget:** accepted. NVIDIA readiness is derived from the complete
   probe plan and is five minutes. Accelerator unavailability is an independent,
   token-keyed, configurable operator-recovery window with a 15-minute default;
   unrelated dispatch progress never resets it.
2. **Claim transfer:** accepted. Endpoint silence never proves ownership death.
   A claim records the Linux boot ID, supervisor PID, process start identity,
   and process group. A live owner blocks replacement. After proven owner death,
   recovery terminates the old process group before transferring the claim.
3. **Co-tenancy:** accepted. Device-global `encoder.stats.sessionCount` is not a
   readiness gate. External encoder sessions may coexist; the declared
   concurrency probe runs alongside them. A failed probe distinguishes external
   contention from a VOOM orphan or an unsupported declaration.
4. **AV1 encode:** accepted. `av1_nvenc` is removed from this slice because
   neither acceptance GPU can execute it. Probed `av1_cuvid` decode remains.
   AV1 encode becomes a hardware-backed follow-up.
5. **Per-file decoder eligibility:** accepted. Run preflight checks the
   backend/encoder and whether hardware decode exists at all, not each selected
   input codec. Unsupported source codecs become per-file planning blocks;
   exact advertised decoder matching remains a per-ticket dispatch condition.

### Implementation-risk review

1. **Migration preservation:** accepted. Migration 0029 performs one
   explicit-column copy including `retired_at` and never reruns seed inserts.
2. **Readiness arithmetic:** accepted. The five-minute deadline is derived from
   18 maximum sequential 15-second stages plus 30 seconds of coordination, and
   timeout errors retain the active probe name.
3. **Unavailable clock ownership:** accepted. A monotonic clock is keyed by
   hardware token and is unaffected by unrelated progress. Its configurable
   15-minute default represents operator/service-manager reaction time.
4. **Identity timing:** accepted. A three-second realtime encode outlives its
   two-second poll window. Early successful exit is inconclusive and retried;
   only an observed different UUID is an identity mismatch.
5. **Encoder-session enumeration:** accepted. The command was verified against
   a live A6000 HEVC session. Unsupported, malformed, or empty enumeration
   degrades attribution only and cannot manufacture a contention diagnosis.
6. **Command evidence ordering:** accepted. Both command shapes ran on both
   acceptance GPUs before golden tests were specified. Verbose NVIDIA-decode
   logs prove CUDA frames reach `scale_cuda` and NVENC without a system-memory
   transition.
7. **ADR control:** accepted. The missing 0048 index row and issue #400 link are
   present. ADR 0049 remains proposed until human design approval; the shipping
   step explicitly accepts it and adds an ADR-index CI guard.

## Non-goals

- QSV, VAAPI, AMF, or VideoToolbox commands or capability vocabulary.
- NVIDIA decode feeding a software encoder.
- H.264 output profiles.
- AV1 NVENC output profiles.
- Automatic discovery of the product's theoretical maximum NVENC sessions.
- Performance comparison against the issue-339 `libx265` baseline.
- Remote-worker GPU configuration; this slice configures the supervisor-owned
  local FFmpeg worker.
- Windows NVIDIA supervision; process ownership recovery in this slice uses
  Linux process identity and process groups.

## Profile contract

`TranscodeVideoProfile` and policy inline settings preserve the existing
software field names. `crf` becomes nullable in Rust and SQLite but remains
present for every software profile. A nullable `cq` field is added:

- software encoders require `crf` and reject `cq`;
- `hevc_nvenc` requires `cq` in `1..=51` and rejects `crf`.

Zero is not accepted even though FFmpeg spells it as automatic; VOOM profiles
must request an explicit quality. NVENC emits deterministic VBR constant
quality:

```text
-c:v hevc_nvenc
-rc vbr
-cq <value>
-b:v 0
-preset <p1..p7>
-tune <hq|uhq|ll|ull|lossless>  # when present
```

Existing `codec_profile`, `codec_level`, `pixel_format`, dimension caps, output
container, and copy-compatible fields remain descriptor-validated. HEVC NVENC
accepts only `yuv420p` and `yuv420p10le` as profile output formats in this
slice. CUDA filters map them to `nv12` and `p010le`, respectively; an omitted
format resolves to `yuv420p`/`nv12`.

The profile gains `decode`, a tagged enum whose default software variant is
omitted during serialization:

```json
{"backend":"nvidia"}
```

The real serde variants are newtypes over `deny_unknown_fields` content
structs, following ADR 0013. The policy DSL and profile CLI accept only
`software` or `nvidia`. NVIDIA decode with a software encoder is rejected in
this slice.

Migration 0029 rebuilds `video_profiles` through
`video_profiles_new`. The new table carries every column present after migration
0021—`id`, `name`, `target_codec`, `encoder`, `crf`, `preset`, `tune`,
`codec_profile`, `codec_level`, `pixel_format`, `max_width`, `max_height`,
`output_container`, `copy_compatible`, and `retired_at`—plus nullable `cq` and a
required decode backend with default `software`. It makes `crf` nullable and
extends the encoder constraint with `hevc_nvenc`.

The migration performs exactly one explicit-column
`INSERT INTO video_profiles_new (...) SELECT ... FROM video_profiles`, assigning
`NULL` to `cq` and `software` to decode for every old row. It then drops the old
table and renames the new table. It does not execute seed inserts: all existing
seed, operator-created, modified, and retired rows come only from the copy.
IDs, names, values, `retired_at`, and row count are preserved. No NVIDIA profile
is seeded; an operator chooses quality, decode mode, and host availability
explicitly.
The profile CLI omits an inapplicable quality field and the default software
decode field, so software envelope shapes do not gain `cq: null` or a decode
object. Upgrade order remains binary before database: an old binary treats a
0029 database as too new, while the new binary reads pre-0029 databases only
through the existing schema-version guard and never writes before `init`
applies the migration. Rollback is database restore plus old binary; there is
no down migration.

## Worker configuration and preflight

The existing software command remains:

```text
voom worker run-local --kind ffmpeg
```

NVIDIA configuration adds:

```text
voom worker run-local --kind ffmpeg \
  --nvidia-device <GPU-uuid> \
  --nvidia-max-sessions <integer 1..=16, default 1>
```

`--nvidia-max-sessions` is valid only with `--nvidia-device`. The control plane
accepts only a full NVIDIA GPU UUID, queries that UUID before registration, and
starts the Linux run-local supervisor in a dedicated process group. The durable
`nvidia:<uuid>` claim records the worker, supervisor PID, Linux process start
identity, process-group ID, boot ID, and declared capacity. The claim is unique
and is released in the same transaction that retires its owner.

Claim recovery checks OS process identity before registry state:

1. if the recorded boot ID differs from the current boot, prior processes are
   gone and no numeric PID or process-group ID is signalled;
2. if the boot matches and the recorded PID still has the recorded start
   identity, the owner is alive; endpoint failure does not permit retirement or
   claim transfer;
3. if the owner is dead but its process group still has members, recovery sends
   bounded TERM then KILL, waits for disappearance, and verifies the group is
   empty;
4. only after owner death and group cleanup may recovery retire the old worker
   and transfer the claim;
5. a reused PID or process-group ID is detected from the recorded start
   identity and is never signalled.

The FFmpeg worker and all FFmpeg probe/dispatch children inherit the dedicated
group. External encoder PIDs never belong to it and are never terminated.

The supervisor launches the worker with
`CUDA_VISIBLE_DEVICES=<GPU-uuid>`. That makes the configured GPU the worker's
only CUDA-visible device; every FFmpeg CUDA option therefore uses device `0`.
No durable contract stores or uses an NVML or CUDA ordinal. Test-only binary
overrides cover `nvidia-smi`, FFmpeg, and FFprobe boundaries.

Before printing its internal bound line, the FFmpeg worker:

1. verifies FFmpeg/FFprobe and the existing required software codecs/muxers;
2. queries `nvidia-smi -i <uuid>` and requires the returned UUID to equal the
   configured UUID while recording the device name and driver version;
3. checks CUDA plus the global NVENC/CUVID/filter lists;
4. launches a three-second, realtime NVENC identity encode under
   `CUDA_VISIBLE_DEVICES`, polling
   `nvidia-smi --query-compute-apps=pid,gpu_uuid` every 100 ms for at most two
   seconds, and requires the FFmpeg PID to appear against the configured UUID;
5. runs a real 256x256 one-frame HEVC encode and fails if it is unusable;
6. creates private one-frame H.264, HEVC, and AV1 fixtures and runs
   exact-device `h264_cuvid`, `hevc_cuvid`, and `av1_cuvid` smoke decodes,
   retaining only successful source codecs;
7. starts `nvidia-smi encodersessions -i <uuid>`, reads its first complete
   snapshot, then terminates and reaps the streaming process; it proves that no
   listed PID belongs to a prior VOOM claim, while external encoder PIDs are
   recorded only for diagnostics;
8. runs the declared number of concurrent HEVC smoke encodes in its own process
   group, even when external sessions exist;
9. fails startup if the declaration cannot be sustained, distinguishing an
   external-contention error from a claim-owned orphan or an unsupported
   declaration.

The identity encode is deliberately resident longer than its poll window. A PID
observed under another UUID is an immediate identity failure. A successful
encode that exits before observation is inconclusive, not a mismatch; startup
retries the complete identity stage up to three times and then reports an
inconclusive probe. A non-zero encode exit remains an encoder probe failure.

Every child probe has a 15-second deadline, kill-on-drop behavior, and an
explicit reap. The preflight planner expands the nine logical steps above into
at most 18 sequential deadline-bearing stages:

| Stages | Work |
|---|---|
| 1–3 | FFmpeg, FFprobe, and existing software inventory |
| 4 | `nvidia-smi` UUID/device identity |
| 5–8 | CUDA, encoder, decoder, and filter inventories |
| 9 | Resident PID-to-UUID identity encode and polling |
| 10 | HEVC NVENC smoke encode |
| 11–13 | H.264, HEVC, and AV1 fixture creation |
| 14–16 | H.264, HEVC, and AV1 CUVID smoke decode |
| 17 | First encoder-session snapshot |
| 18 | Concurrent declared-capacity group |

The capacity group counts once even at the maximum declaration of 16 because
its children run concurrently.

`NVIDIA_PREFLIGHT_STAGE_TIMEOUT` is 15 seconds,
`MAX_NVIDIA_PREFLIGHT_STAGES` is 18, and the supervisor reserves 30 seconds for
spawn, pipe, parse, cleanup, and scheduling overhead. The derived NVIDIA
readiness deadline is therefore five minutes:

```text
18 * 15 seconds + 30 seconds = 300 seconds
```

These constants and the tagged progress record live in the shared worker
protocol and are consumed by both the FFmpeg worker and local supervisor; the
deadline arithmetic has one source of truth.

The supervisor pipes tagged preflight progress from worker stderr, forwards it
to its own stderr, and retains the last `probe=<name> state=started` record. On
readiness timeout it kills and reaps the child and includes that probe name in
the error. The existing software path keeps its ten-second deadline. A compile-
time/unit invariant requires the NVIDIA readiness deadline to be strictly
greater than the maximum summed stage budget.

Probe artifacts live under a mode-0700 temporary directory that is removed on
success and error.
The capacity probe establishes that the declaration is concurrently usable
under the current FFmpeg build, driver, and observed co-tenant load. External
sessions do not block readiness when all VOOM probes succeed. If they leave
insufficient capacity, startup fails retriably with their session PIDs rather
than misreporting a VOOM ownership leak or a permanently invalid declaration.

Encoder-session enumeration is diagnostic, not the claim-cleanup authority;
Linux process identity and process-group emptiness own that decision. If
`nvidia-smi encodersessions` exits non-zero, reports `Not Supported`, is
malformed, or returns no rows, startup records attribution as unavailable and
still runs the capacity probe. A successful capacity probe permits readiness.
A failed probe without trustworthy enumeration reports that declared
concurrency was unavailable but external attribution was unavailable; it does
not invent external PIDs or misclassify the failure as a VOOM orphan.

The internal readiness line retains `BOUND addr=<socket>` and adds one compact
JSON capability token for GPU workers. The local supervisor parses the token,
records `nvidia:<uuid>` in `worker_capabilities.hardware`, records the typed
descriptor under `extra.video_accelerator`, and sets the `transcode_video`
grant limit to the tested session count. Software readiness has no suffix and
retains empty hardware.

## Scheduling and capacity

The planner includes the observed source codec in the transcode operation
payload. The worker request includes it only when NVIDIA decode is selected.
The worker's input probe must match before choosing `<codec>_cuvid`. This slice
accepts only H.264, HEVC, and AV1 source codecs for NVIDIA decode and maps them
to `h264_cuvid`, `hevc_cuvid`, and `av1_cuvid`.

The shared profile derives a `VideoHardwareRequirement`:

- software encoder + software decode: no accelerator;
- NVENC + software decode: NVIDIA backend plus exact encoder;
- NVENC + NVIDIA decode: NVIDIA backend, exact encoder, and CUVID decoder for
  the observed source codec.

Stored profile references are already resolved before the existing policy-tool
preflight. That preflight additionally derives every distinct profile-level
video requirement before opening a job. It requires an effective,
endpoint-reachable, identity-verified worker for each software or
backend/encoder requirement. A software requirement matches only an
unaccelerated FFmpeg worker. An NVIDIA decode profile also requires that a
matching HEVC NVENC provider advertise at least one usable CUVID decoder, but
preflight does not cross-product the policy with selected input codecs. A
GPU-only FFmpeg deployment therefore fails preflight for a software profile
with guidance to start an ordinary software worker. A missing NVIDIA backend or
HEVC encoder fails once for the run rather than once per file.

Per-file planning owns source-codec compatibility. For NVIDIA decode, H.264,
HEVC, and AV1 source codecs produce an exact decoder requirement in that file's
ticket. Any other observed codec produces a typed per-file planning block
naming the codec and profile; it does not reject the run or block independent
files. For a recognized codec, absence of an exact advertised decoder becomes a
terminal `MissingCapability` failure on only that transcode ticket. ADR 0039
then applies the policy's ordinary abort/continue semantics.

Worker candidate projection parses the typed capability. Software requirements
accept only candidates without an accelerator descriptor. NVIDIA requirements
require a matching descriptor and usable codec lists. Incompatible candidates
remain visible to diagnostic scoring with typed reasons; they are never passed
to dispatch. After selecting an NVIDIA candidate and before acquiring its
lease, the executor repeats the endpoint handshake and identity check. An
unreachable or stale endpoint enters the accelerator-unavailable path rather
than consuming an attempt.

For a compatible NVIDIA candidate, active sessions count every held
`transcode_video` lease whose selected worker advertises the same
`nvidia:<uuid>` token. This intentionally aggregates duplicate worker
incarnations. Candidate selection uses that count; equal utilization resolves
by worker ID.

Every live descriptor for the same hardware token must declare the same tested
capacity. The local-device claim prevents that state in the supported
run-local path. Policy preflight rejects a conflicting durable state before
opening a job and names every worker and declaration. If a conflict appears
during an active run, candidate projection returns a typed capability
rejection; it does not return a repository error. The executor records a
terminal `MissingCapability` failure only for the affected `transcode_video`
ticket, while unrelated tickets and operations continue according to the run's
failure mode.

Lease acquisition remains the authority:

1. parse and validate the ticket profile and source codec;
2. re-read the selected worker's typed capability;
3. require exact backend/encoder/decoder compatibility;
4. count held leases for the stable hardware token;
5. return the same typed per-ticket capability rejection if live descriptors
   disagree on capacity;
6. compare the count against that device capacity;
7. insert the lease only when capacity remains.

The savepoint rollback and `BEGIN IMMEDIATE` store-owned capacity behavior from
#379 remains in force. A compatibility rejection is terminal pre-lease failure.
Device capacity saturation uses the existing typed capacity-deferred behavior
and does not consume an attempt.

When candidate projection finds no currently compatible NVIDIA worker, it also
checks durable accelerator history:

- no descriptor has ever matched the backend, encoder, and decoder: preserve
  the attempt-consuming `NO_ELIGIBLE_WORKER` backstop;
- a descriptor matched but its worker is now absent or unreachable: return an
  accelerator-unavailable deferral, with no ticket mutation, event, or attempt.

The unavailable path refreshes candidates and the runtime registry at the
existing 250 ms capacity-retry interval so a replacement worker incarnation can
resume the run.

`RunLoopState` owns an accelerator-unavailable monotonic clock keyed by stable
hardware token. The clock starts when a previously matching descriptor first
becomes absent, stale, or endpoint-unreachable. It resets only when an eligible
descriptor for that token reappears and passes endpoint identity. Unrelated
dispatches, active leases, capacity waits, and other hardware tokens neither
start nor reset it. Expiry is checked on every run-loop turn, including while
unrelated tickets remain active.

For a pending requirement with several historically compatible tokens, every
absent token keeps its own clock. A currently eligible token dispatches
immediately. While all are absent, the ticket remains deferred until every
compatible historical token has expired; a newly registered compatible token
also resumes dispatch immediately. An unused absent token never fails a job by
itself.

The timeout is independent from the job-wide one-minute device-capacity clock.
`compliance execute` exposes
`--accelerator-unavailable-timeout-seconds`; it must exceed the five-minute
NVIDIA readiness deadline and defaults to 900 seconds. VOOM does not
automatically restart run-local supervisors, so this is explicitly an
operator/service-manager reaction budget plus replacement readiness, not a
derived hardware constant. On expiry the executor stops new dispatch, drains
already active work through the existing fatal path, and fails the job with an
actionable `NO_ELIGIBLE_WORKER` message. The unavailable ticket and its attempt
remain unchanged. New runs normally cannot reach either dispatch backstop
because policy preflight requires a live matching worker.

Selection returns a `VideoHardwareAssignment` with the backend, stable hardware
token, and UUID. The selected worker ID remains the lease owner, while the
profile and descriptor retain the required encoder, decoder, device name, and
driver version. Immediately before dispatch, the operation adapter adds that
exact assignment to the hardware-only request. The FFmpeg worker rejects a
request whose token or UUID differs from its startup descriptor. The direct
one-shot bundled dispatcher has no scheduler assignment and rejects hardware
profiles with guidance to use a configured run-local worker; it remains
unchanged for software profiles.

## FFmpeg command shapes

Software decode plus NVENC:

```text
CUDA_VISIBLE_DEVICES=<GPU-uuid>
-i <input>
...maps...
-vf format=<nv12|p010le>,hwupload_cuda=device=0[,scale_cuda=...]
-c:v hevc_nvenc -rc vbr -cq <n> -b:v 0 ...
```

NVIDIA decode plus NVENC:

```text
CUDA_VISIBLE_DEVICES=<GPU-uuid>
-hwaccel cuda
-hwaccel_device 0
-hwaccel_output_format cuda
-c:v <source_codec>_cuvid
-i <input>
...maps...
[-vf scale_cuda=...:format=<nv12|p010le>]
-c:v hevc_nvenc -rc vbr -cq <n> -b:v 0 ...
```

The implementation derives one filter graph so format conversion and scaling
cannot emit competing `-vf` flags. Hardware output facts are still checked by
FFprobe through the current container/codec/dimension/pixel-format contract.
`-gpu` is intentionally absent: both command shapes supply a CUDA hardware
frames context, which is the authoritative NVENC device selection.

## Acceptance-host command evidence

These commands were executed before implementation against FFmpeg 8.1.2,
NVIDIA driver 595.80, an RTX A6000, and a Quadro RTX 4000. The validated argv
above is the source for Step 4's golden fixtures.

With a realtime HEVC NVENC process active on each GPU,
`nvidia-smi encodersessions -i <uuid>` returned the live process IDs:

```text
# GPU Session    Process   Codec       H       V Average     Average
# Idx      Id         Id    Type     Res     Res     FPS Latency(us)
    0      35    2454698   H.265     256     256       0           0
    1      36    2454962   H.265     256     256       0           0
```

The simultaneous compute-app query returned the same PID and configured UUID:

```text
2454698, GPU-b99cfc2b-af73-e1a0-996d-232b6955bad9, ffmpeg
2454962, GPU-424c9d31-0662-40ce-4965-b4c886f9e38a, ffmpeg
```

The three-second identity probe was observed under the configured UUID on both
devices, and repeated PID-to-UUID observation succeeded 10/10:

```text
identity verified: pid=2233030 uuid=GPU-b99cfc2b-af73-e1a0-996d-232b6955bad9
identity verified: pid=2233174 uuid=GPU-424c9d31-0662-40ce-4965-b4c886f9e38a
```

Both software-decode/upload and NVIDIA-decode command shapes produced the same
facts on both GPUs:

```text
A6000 software: hevc,512,512,yuv420p
A6000 hardware: hevc,512,512,yuv420p
RTX 4000 software: hevc,512,512,yuv420p
RTX 4000 hardware: hevc,512,512,yuv420p
```

Verbose NVIDIA-decode output proved CUDA frames remained on device through
decode, `scale_cuda`, and NVENC:

```text
Formats: Original: cuda | HW: cuda | SW: nv12
graph input ... pixfmt:cuda
Parsed_scale_cuda ... fmt:nv12 -> ... fmt:nv12
Using input frames context (format cuda) with hevc_nvenc encoder.
```

The NVIDIA-decode logs contained no `hwdownload`, `hwupload`, or `auto_scale`
filter and emitted no warnings. Step 5 repeats these commands through VOOM and
treats any such system-memory transition as an acceptance failure.

## Result and durable evidence

`TranscodeVideoRequest` gains an optional typed hardware assignment containing
the backend, stable hardware token, and UUID. The profile and selected worker
descriptor carry the encoder and optional decoder requirements. The assignment
is omitted for software requests. `TranscodeVideoResult` echoes the same
assignment. The control plane rejects:

- hardware evidence on a software profile;
- missing evidence on a hardware profile;
- wrong backend, hardware token, or device identity.

The result assignment must equal the request assignment, not merely name a
compatible NVIDIA device. The startup PID-to-UUID readback independently proves
that the CUDA-isolated worker uses that physical GPU.

The optional assignment is copied into the transcode success event and
execution report. Started events remain unchanged because no worker owns the
work yet. Existing artifact verification, commit, lineage, and media-snapshot
behavior is unchanged.

## Failure behavior

- Missing `nvidia-smi`, driver, device, or permission: worker startup dependency
  error naming the binary/device and corrective configuration.
- A process-live local supervisor claims the UUID: start rejection naming its
  PID and worker, even if its endpoint is unreachable; stop that owner before
  retrying.
- A dead claim owner left child processes: bounded process-group termination and
  verification before retirement or claim transfer; cleanup failure preserves
  the old claim and names the remaining PIDs.
- External NVENC sessions: allowed when all declared VOOM probes succeed;
  otherwise a retriable contention error names the external encoder PIDs and no
  capability is advertised.
- FFmpeg lists an encoder but the device smoke test rejects it: omit that
  encoder; fail startup if none remain.
- Declared concurrency fails: startup error with requested capacity and the
  first failed probe.
- Missing compatible provider at run start: policy preflight failure naming the
  backend/encoder/decoder or the missing software worker; no job is opened.
- Previously matching device temporarily absent during a run: token-keyed,
  non-attempt-consuming deferral with runtime refresh; the configurable
  15-minute default permits operator or service-manager replacement. Timeout
  fails the job without terminally failing the ticket.
- No matching device has ever been advertised after a preflight bypass:
  attempt-consuming `NO_ELIGIBLE_WORKER`; no software fallback.
- Conflicting live capacity declarations: policy preflight blocks a new run;
  a mid-run conflict terminally rejects only the affected transcode ticket as
  `MissingCapability`, never as a job-fatal projection error.
- Device becomes full after selection: typed capacity deferral, ticket and
  attempt unchanged.
- Device or worker disappears after lease acquisition: ordinary typed worker
  failure naming the configured UUID; a retry never changes encoder/decode
  mode. Pre-lease disappearance uses the non-attempt-consuming unavailable
  path above.
- Source codec differs from the planned expectation: malformed request/source
  mismatch before FFmpeg.
- Unsupported NVIDIA-decode source codec: per-file planning block; other files
  remain eligible.
- Worker reports another GPU or omits assignment: malformed worker result.

## Platform decomposition

Follow-up issues will reuse only the backend-neutral profile requirement,
capability descriptor, scheduler matching, result assignment, and
UUID/device-capacity seams proven here. Each issue owns its native probe and
FFmpeg filter/codec commands:

- NVIDIA AV1 encode on AV1-capable hardware
- Intel Quick Sync Video (QSV)
- Linux VAAPI
- AMD AMF
- Apple VideoToolbox

Each issue requires development and real-media acceptance on matching hardware.
