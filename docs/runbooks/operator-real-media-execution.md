# Runbook: Operator Real-Media Execution

Run the real-media pipeline through an authenticated storage-owner node agent:
scan a library, author a policy, and execute remux-to-MKV + transcode-to-HEVC.
This is the operator procedure behind the Sprint 17 slice
(`docs/superpowers/specs/2026-06-05-voom-sprint-17-slice-operator-execution-design.md`).

## Prerequisites

Use an initialized production node configuration from
[operator-node-agent.md](operator-node-agent.md), or the dedicated local acceptance
stack below. The local helper requires Python 3.13 or newer, `jq`, `ffmpeg`,
`ffprobe`, `mkvmerge`, and the repository's pinned Rust toolchain. Input, staging,
and output directories must be separate and must not contain one another.
Choose input media and output retention before starting; promotion is add-only.

## Procedure

All commands emit a single JSON envelope on stdout; logs go to stderr.
`run-local` is the documented exception — its stdout is a two-line contract
(readiness line, then the retirement envelope on shutdown). See the
[run-local stdout contract](#run-local-stdout-contract) note below.

### 1. Build the local acceptance binaries

From the repository root:

```sh
cargo build -p voom-cli -p voom-api -p voom-node-agent -p voom-scan-worker -p voom-hash-worker \
  -p voom-ffmpeg-worker -p voom-ffprobe-worker \
  -p voom-mkvtoolnix-worker -p voom-verify-artifact-worker
export PATH="${CARGO_TARGET_DIR:-$PWD/target}/debug:$PATH"
```

`voom init` is the sole
schema-creation/migration entrypoint; the helper invokes it for a new database.
Other commands only connect to an existing database.

### 2. Start the authenticated local stack

In terminal A, choose a fresh acceptance work directory and existing input directory:

```sh
python3 scripts/acceptance-bootstrap.py serve \
  --workdir /var/tmp/voom-acceptance \
  --input /path/to/input \
  --staging /path/to/staging \
  --output /path/to/output
```

Keep stdin open. Wait for the single `status: "ready"` JSON record, which gives
`node_id`, `library_id`, worker IDs and the input/staging/output root IDs. The helper
registers the node and roots, sets staging/output defaults, starts the real API on
an allocated loopback port and starts the owner agent with six real workers. Readiness
requires authenticated activation of every configured root and all six active workers.
It stops before scanning if activation fails. This cleartext loopback stack is for
local acceptance; use the node-agent runbook's authenticated deployment for remote hosts.

In terminal B, use the same work directory and the node/root IDs from readiness:

```sh
export VOOM_DATABASE_URL=sqlite:///var/tmp/voom-acceptance/voom.db
export VOOM_LOCAL_NODE_ID=<node_id>
```

Exact local node identity is required for the current promotion path. Registration
stdout is captured in memory; the bearer goes into a private temporary credential
outside `bootstrap-evidence`, and is removed when the stack stops. Nonsecret evidence,
databases and media outputs remain for inspection. A fresh helper invocation requires
a fresh work directory; it does not resume an earlier registration.

For accelerated media execution, configure the authenticated owner agent's accelerator
manifest using [operator-node-agent.md](operator-node-agent.md). The local helper
accepts `--accelerator-json` from a successful bundled-worker readiness probe; the
VAAPI and VideoToolbox acceptance scripts demonstrate that setup. Separate standalone
`worker run-local` supervisors are used only for their dedicated supervisor checks.

An NVIDIA-bound worker advertises only GPU video work; it does not replace the
unbound worker needed by software profiles, audio transcoding, and extraction.
The session declaration is per physical UUID and must be in `1..=16`. Startup
pins CUDA visibility to that UUID, independently proves the FFmpeg PID-to-UUID
mapping, executes HEVC NVENC and per-decoder smoke probes, and proves the declared
concurrency before the worker becomes ready. A second live supervisor cannot
claim the same UUID.

If an advertised device disappears during a run, its tickets remain ready and
do not consume attempts for 15 minutes. A replacement worker for the same UUID
can resume them. Capacity saturation uses the shorter one-minute capacity wait.
If the 15-minute device-recovery window expires, the job fails with the hardware
token in the error while unrelated in-flight work is drained.

To repeat the repository's real-device acceptance on every installed NVIDIA
accelerator:

```
scripts/accept-nvidia-video-acceleration.sh \
  GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee \
  GPU-ffffffff-1111-2222-3333-444444444444
```

The script runs the worker's identity/capability preflight and executes both
supported graphs on each UUID: software decode plus `hwupload_cuda`, and H.264
CUVID decode plus `scale_cuda`, each ending in HEVC NVENC. AV1 NVENC is not part
of this slice; the worker may advertise AV1 CUVID decode only when that exact
device passes its startup probe.

#### AMD VAAPI HEVC encoding

Declare each VAAPI worker on the owner agent using its probed accelerator descriptor.
The configured identity is its **PCI address**.

Read the address off `lspci -D` (`0000:f4:00.0` is domain:bus:device.function,
lowercase hex). Configuration accepts that and nothing else — **not**
`/dev/dri/renderD128` and not an ordinal. Render-node numbers are assigned by
enumeration order and renumber across a reboot or a driver change; the PCI
address behind them cannot. Startup resolves
`/dev/dri/by-path/pci-<addr>-render`, reads the resolved node's own address back,
and refuses to start on a disagreement.

The worker's user must be able to open the render node. On Fedora that means
membership in `render`; some hosts own the node through `video` instead:

```
sudo usermod -aG render "$USER"   # log out and back in, or use `newgrp render`
ls -l /dev/dri/by-path/pci-0000:f4:00.0-render
```

The declared session count is per physical device and must be in `1..=16`
(default 1). Startup binds the node, probes an HEVC encode and one decode per
candidate codec on that node, and runs the declared number of encodes on it
before the worker becomes ready. Read that last step precisely: it spawns every
session before reaping any and fails startup if any of them fails, but each probe
encodes a single frame, so it does not *prove* that two sessions were ever open
at the same instant. A device that cannot sustain the declared count can
therefore still pass startup and degrade later under load — declare a count the
device is known to support rather than treating readiness as a guarantee of it
(`docs/debt/0002-vaapi-capacity-probe-does-not-prove-sessions-overlap.md`).

A VAAPI-bound worker advertises only VAAPI video work; it does not replace the
unbound worker needed by software profiles, audio transcoding, and extraction.
The advertised hardware token is `vaapi:pci-<addr>`.

Running the bundled worker binary directly — as the acceptance script and the
tests do — uses these environment seams instead of the CLI flags:

| Variable | Meaning |
|---|---|
| `VOOM_VAAPI_DEVICE` | PCI address to bind, e.g. `0000:f4:00.0`. Required. |
| `VOOM_VAAPI_MAX_SESSIONS` | Declared concurrent sessions, `1..=16`, default 1. |
| `VOOM_DRI_ROOT` | Render-node root, default `/dev/dri`. |
| `VOOM_DRM_SYSFS_ROOT` | Sysfs DRM root for the address readback, default `/sys/class/drm`. |
| `VOOM_FFMPEG_BIN` / `VOOM_FFPROBE_BIN` | Override the tools the worker executes. |

##### HEVC encode needs a non-stock VA driver

Stock Fedora's `mesa-dri-drivers` advertises exactly **one** VAAPI encode
entrypoint: AV1. It cannot encode HEVC or H.264. HEVC encode requires RPM
Fusion's `mesa-va-drivers-freeworld`:

```
sudo dnf install mesa-va-drivers-freeworld
```

Do not assume a stock Fedora install can drive a `hevc_vaapi` profile — it
cannot, and the failure surfaces as `No usable encoding profile found`. AV1
encode and **all** hardware decode (`h264`, `hevc`, `av1`) work on stock Mesa.

Installing the freeworld package puts `/usr/lib64/dri-freeworld` on the global
library path and makes it the system-wide default; reverting needs an explicit
`LIBVA_DRIVERS_PATH=/usr/lib64/dri`. Advertised capability therefore tracks the
*loaded driver build*, which is invisible from the render node and from FFmpeg's
encoder list. That is why the worker proves each codec with a real encode at
startup and never caches the result across restarts: a host driver change can
move capability with no voom configuration change.

##### Main10 is proven by acceptance, not by the startup probe

The startup probe encodes 8-bit (`nv12`) only. The descriptor has nowhere to
record "Main10 proven", so probing 10-bit could only be a hard startup
requirement, which would refuse an otherwise usable 8-bit device. A worker that
became ready therefore guarantees `hevc_vaapi` 8-bit on that device and **does
not** guarantee Main10. Main10 is verified end-to-end by
`scripts/accept-vaapi-video-acceleration.sh` (below); run it before relying on a
`p010` profile on a host whose driver build you have not previously exercised.

##### A VAAPI profile cannot downscale

There is no verified `scale_vaapi` command shape in this slice, so a VAAPI
profile that sets `max_width`/`max_height` **fails, per file, on every source
that exceeds the cap** — where the equivalent software profile would downscale.
Both are ordinary policy fields, so this combination is easy to author by
accident. Either omit the dimension caps from a VAAPI profile, or scope it to
sources already within them. Silently ignoring the cap would emit output that
violates the profile, so the failure is deliberate.

##### `copy_compatible` genuinely stream-copies, and still needs the device

A `copy_compatible` VAAPI profile does perform a real `-c:v copy` when the source
already conforms: the worker compares the source's *file* pixel format against
the format the profile's surface would write, not against the surface name. The
worker still requires the VAAPI assignment for that copy, because the scheduler
leased that device for that ticket. Do not expect a copy-only VAAPI profile to
run on an unbound worker.

##### `pixel_format` is load-bearing on a VAAPI profile

`pixel_format` is how 10-bit is requested (`p010`; `nv12` is 8-bit) and it is
what decides whether a produced artifact is judged compliant on the next run.
Treat it as required, not optional. It names a GPU **surface** format, while the
encoded file reports a **file** format:

| Profile `pixel_format` | `codec_profile` | The file ffprobe reports |
|---|---|---|
| `nv12` | `main` (or unset) | `hevc` / `Main` / `yuv420p` |
| `p010` | `main10` (or unset) | `hevc` / `Main 10` / `yuv420p10le` |

Both mismatched pairings are rejected when the profile is created, not at encode
time: `main` with `p010` (an 8-bit profile cannot carry a 10-bit surface) and
`main10` with `nv12` — including `main10` with no `pixel_format` at all, since
absent means `nv12`. Left to FFmpeg, each of them reports only `No usable
encoding profile found`.

##### With `decode: vaapi`, the surface must match the source's bit depth

A hardware-decoded source stays in GPU frames all the way to the encoder — that
is the point of the mode, and it is why the command carries no filter at all. So
nothing can convert the frame in flight, and the profile's surface must already
match the source's depth:

| Source | Profile |
|---|---|
| 8-bit (`yuv420p`) | `nv12` + `main` |
| 10-bit (`yuv420p10le`) | `p010` + `main10` |

A mismatch is refused before FFmpeg runs, naming both formats. Left to FFmpeg it
reports only `No usable encoding profile found`, which reaches the operator as a
worker crash wrapping an FFmpeg dump.

This constraint is specific to `decode: vaapi`. A software-decoded source uploads
through `format=<surface>,hwupload`, which converts the frame on the way to the
device, so a mixed-depth library transcodes to a single output depth — use
software decode when that is what you want.

##### Startup diagnostics

Every VAAPI preflight failure names the PCI address and has one operator action:

| Diagnostic | What to do |
|---|---|
| `VAAPI device must be a PCI address like 0000:f4:00.0` | Replace a node path or ordinal with the lowercase `lspci -D` address. |
| `PCI address ... has no VAAPI render node: ... does not exist` | Confirm the address with `lspci -D` and that a DRM driver bound the device. |
| `VAAPI render node is absent for PCI address ...` | The device was removed or its driver unbound; re-check `lspci -D`. |
| `... is not a character device` | Something other than a DRM render node occupies that path. |
| `permission denied opening VAAPI render node ...` | Add the worker's user to `render` (or `video` on hosts that own the node that way). |
| `... reports PCI address X but configuration names Y` | The `by-path` symlink is stale: `udevadm trigger`, or correct the configured address. |
| `the loaded VA driver build cannot encode hevc_vaapi ...` | Install a driver build carrying HEVC encode — on Fedora, `mesa-va-drivers-freeworld`. |
| `hevc_vaapi probe encode ... failed: <ffmpeg error>` | Read FFmpeg's own message; the driver has HEVC but the encode did not run. |
| `VAAPI capacity probe for N concurrent ... failed` | VAAPI exposes no session enumeration, so the cause cannot be attributed: lower `--vaapi-max-sessions` or retry when the device is idle. |
| `VAAPI readiness deadline of 300 seconds expired before <stage>` | A probe hung; the named stage is where. Check for a wedged FFmpeg process on the node. |

One VAAPI startup diagnostic is **not** a failure — the worker still binds and
becomes ready:

| Diagnostic | What to do |
|---|---|
| `VAAPI decode probe on PCI address ... did not prove <codec>: <reason>` | That codec was not proven on this driver build, so it is not advertised and sources in it will not be hardware-decoded — they still transcode with software decode. Expected on a build lacking the codec; if you expect it, read the quoted FFmpeg reason and check the driver build. |

This is the signal to watch after a host driver change. ADR 0052 §2 probes on every
start precisely because a driver swap moves capability with no VOOM configuration
change, and a codec that stops proving otherwise shows up only as candidates
disappearing.

##### Real-device acceptance

```
scripts/accept-vaapi-video-acceleration.sh 0000:f4:00.0
```

Per device, this binds a worker and asserts the readiness line names the
configured address, then runs three real `compliance execute` pipelines on that
device — 8-bit `nv12`/Main, 10-bit `p010`/Main10, and a VAAPI-decoded source —
reading each produced file back with ffprobe. It then plans the same policy over
each produced artifact in a database that never saw the source and requires the
planner to want nothing, and checks that no software encoder appears in any argv
the worker executed. It prints one `PASS`/`FAIL` line per check, exits non-zero
on any failure, and leaves its evidence directory in place. It is not part of
`just ci`: it needs the hardware.

#### Apple VideoToolbox encoding

On Apple silicon macOS, declare a host-scoped VideoToolbox worker on the owner
agent using its probed descriptor and an explicit session capacity.

The session declaration must be in `1..=16` and defaults to one. Startup
requires FFmpeg's `videotoolbox` accelerator, `h264_videotoolbox` and
`hevc_videotoolbox` encoders, and `scale_vt`. It executes H.264, HEVC, and AV1
decode pipelines for the supported eight- and ten-bit formats, then proves
every declared homogeneous concurrency group with first-frame and all-live
evidence. Inventory text alone never establishes readiness.

The supervisor and worker independently hash the normalized
`IOPlatformUUID`. Only the lowercase SHA-256 resource ID and
`videotoolbox:<resource-id>` token are persisted or logged. Raw platform UUID,
serial number, hardware UUID, and user name are not part of the contract.

Run host acceptance with an optional declared capacity:

```
scripts/accept-videotoolbox-video-acceleration.sh 4
```

The acceptance command checks real hardware pipelines, the run-local
readiness/retirement stdout contract, the durable host claim, descriptor
capabilities, identity non-disclosure, and clean claim removal.

VideoToolbox profiles use `bitrate_kbps`, the `default` preset, and an accepted
profile/pixel-format pair. `decode: video_toolbox` is explicit; no failure path
falls back to software encode or decode. Hardware decode rejects source/output
bit-depth mismatches and uses `scale_vt` only when downscaling.

On the same boot, claim recovery is conservative: any process at the recorded
PID, any member of the recorded process group, or any inspection failure
preserves the claim for manual investigation. A different boot session proves
the old processes are gone. The supervisor never signals an ambiguous macOS
process group.

Common startup failures include an Intel Mac or non-macOS host, a missing or
incompatible FFmpeg/FFprobe binary, a failed encoder/decoder/format pipeline,
an unprovable declared capacity, a supervisor/worker resource mismatch, or an
existing claim whose prior owner cannot be proven absent. Each fails before
readiness.

The owner agent starts scan, hash, probe, transform and verification workers as
out-of-process children. Wait for its ready workers and effectively available roots
before scanning or executing. For a one-second software-only proof:

```sh
python3 scripts/acceptance-bootstrap.py smoke --workdir /var/tmp/voom-smoke
```

It must publish one observation and one policy member, then stop the API and agent
and remove the temporary credential. Hardware scripts use the same owner bootstrap;
VideoToolbox keeps claim/crash/retirement checks in a separate database.

### 3. Scan the library

```
voom scan --root <storage-root-id>
```

Requests a durable scan run and polls it to a terminal state (`--no-wait`
exits after the request). The bytes are read by the storage-owner node's
agent, so that node's config must declare `scan_library`, `hash_file`, and
`probe_file` workers (see `operator-node-agent.md`); publication happens when
the session completes. Non-media files (unsupported extensions) are excluded
at discovery.

### 4. Author and accept the policy

```
voom policy create \
  --slug remux-to-mkv-and-transcode-to-hevc \
  --file remux-and-hevc.voom
```

Capture `version_id` from the envelope (this is the accepted version). The slug
must match the policy identity compiled from the document; for the sample policy,
that slug is `remux-to-mkv-and-transcode-to-hevc`. The sample policy
(`crates/voom-control-plane/tests/fixtures/policies/remux-and-hevc.voom`):

```
policy "remux to mkv and transcode to hevc" {
  phase remux {
    container mkv
  }
  phase transcode {
    depends_on: [remux]
    transcode video to hevc
  }
}
```

Two phases, applied as barriers across files (ADR-0007): every file is remuxed in
the remux phase, then every file is transcoded in the transcode phase, with the
transcode operating on the remuxed output. Files already compliant for a phase are
skipped.

**`policy create` is not idempotent** — `policy_documents.slug` is `UNIQUE`. On a
re-run, either `voom policy list` to find the existing document id and
`voom policy version add --document-id <id> --file <f>`, or choose a new slug.

#### Sample policy catalog

The two-phase policy above is the minimal example. A set of committed samples in
`crates/voom-control-plane/tests/fixtures/policies/` exercises the full V1(+V1.1)
vocabulary; each has a planner-oracle test in
`crates/voom-control-plane/tests/sample_policies_plan.rs` pinning what it plans:

| Sample | What it does |
|--------|--------------|
| `container-normalize.voom` | Remux every file to mkv; already-mkv files are a no-op. |
| `language-cleanup.voom` | Keep only the preferred-language audio/subtitles, then order tracks and set filter-addressed defaults. |
| `reference-user.voom` | The whole-library flagship: mkv + HEVC video + E-AC-3 5.1 audio + a synthesized stereo downmix + language-filtered keep + filter-addressed defaults + `verify artifact`, run as a five-phase barrier chain. |
| `verify-heavy.voom` | An artifact verification between each mutating phase. |

For a real whole-library run, `reference-user.voom` is the closest to a
production policy. Its language filters
(`keep audio where language in ["eng", "und"]`,
`transcode audio to eac3 where language in ["eng", "und"]`) behave predictably on a messy
library (ADR 0021):

- **Untagged audio is treated as `und`.** A file whose audio carries no language
  tag matches the `und` clause instead of blocking; the plan carries a per-file
  `untagged_track_language_defaulted` warning so you can see which files were
  defaulted.
- **A no-matching-language file fails per file, never silently.** If a file's
  only audio is a language the policy does not keep, the language-filtered audio
  transcode is blocked for that one file and a remux that would strip its last
  audio track is rejected at execution (`no audio track survived the track
  filters`) — surfaced as a `terminal_failure` issue for that file while the rest
  of the library proceeds. voom never emits an audio-less artifact.

### 5. Build an input set for the activated root

```
voom policy input create-from-scan --root <storage-root-id> --slug lib1
```

Builds one input set covering live **video** file-versions under this root; non-video /
unprobeable files are skipped (the envelope reports `included_count` /
`skipped_count`). Capture `input_set_id`.

Policy-input creation accepts at most 10,000 aggregate members and 32 MiB of
serialized draft data. An ordinary whole-scan input uses one member for its
fixture label, leaving room for 9,999 video snapshots. An over-budget request
fails with `POLICY_VALIDATION_ERROR` before any input-set rows are written.
Split a larger or fact-heavy library into root-scoped inputs:

```
voom policy input create-from-scan --root <library_root_id> --slug lib1-root1
```

### 6. Execute

```
voom compliance execute \
  --policy-version-id <version_id> \
  --input-set-id <input_set_id> \
  --max-in-flight-files 4 \
  --staging-root /path/to/staging \
  --output-dir /path/to/output
```

## Output, re-runs, and partial failure

- **Only final artifacts land in `--output-dir`.** Each admitted file advances
  through its phases independently. Its terminal chain tip is promoted as soon
  as that file finishes, then superseded intermediates are removed from staging
  before the slot is refilled.
- **The file window bounds staging residency.** `--max-in-flight-files` defaults
  to 4. Worker capacity still controls operation concurrency; this option limits
  how many file pipelines may retain staged artifacts at once.
- **Add-only.** Promotion never overwrites. Source files are never modified. If a
  destination in `--output-dir` already exists, the run fails rather than
  overwrite.
- **A real-library run can partially succeed** (some files committed, some
  failed). Read partial state with `voom compliance report --job-id <job_id>`.
  To resume, stop the prior executor and run the original `compliance execute`
  command with `--resume-job-id <prior_job_id>`. Keep the same accepted policy
  version, input set, file window, staging/output roots, and safety/backup options.
  Use the latest attempt's job ID from its success or partial-error data.
  Resume opens a new job and reconciles the recorded per-file phases: completed
  work and verification evidence are carried forward rather than repeated.
  The caller must ensure the prior executor is stopped; the flag does not fence
  a concurrent executor. Omitting the flag starts fresh execution.
- **`verify artifact` is a durable read-only phase.** A successful phase reports
  `outcome: "verified"` without advancing the file version. Both
  `compliance execute` and `compliance report --job-id` include the persisted
  verification id, expected and observed facts, worker, status, and any failure
  code. A resumed run reuses that evidence instead of verifying the same phase
  again.
  In that report, `file_phases[*].outcome` and produced artifact IDs are the
  execution results. `phases[*].report` is the compliance snapshot captured for
  that phase, and `latest_phase_index` points at the highest-ordinal phase
  snapshot. A completed file phase can therefore carry an earlier
  `noncompliant` check that explains why work was planned; use the file-phase
  outcome and produced IDs to confirm what committed.
- **Empty / all-non-video scan:** the input set is empty and `execute` is a no-op
  reporting zero planned / zero committed.
- **Scale:** size staging for intermediate and terminal artifacts across at most
  `--max-in-flight-files` active pipelines, plus temporary worker output.
  Transcodes are long-running.

For example, resume the latest stopped attempt with its original settings:

```sh
voom compliance execute \
  --policy-version-id <version_id> \
  --input-set-id <input_set_id> \
  --resume-job-id <prior_job_id> \
  --max-in-flight-files 4 \
  --staging-root <original_staging_root> \
  --output-dir <original_output_root>
```

Phase/ticket references and verification evidence survive resume. The
`audio_extract_outputs` and `audio_synthesis_companions` arrays keep their
current-job meaning; they do not aggregate outputs from earlier jobs.

## Mid-run monitoring

A whole-library `compliance execute` can run for hours or days. The database is
opened in WAL mode, so a second `voom` process can read the same database
read-only while the run is in flight — reads never block the running writer.

- **Live signals (while `execute` is running):**
  - `voom worker list` — the workers currently registered and leased for the run.
    This concurrent read against the live database is exercised by the operator
    execution e2e test (`crates/voom-cli/tests/operator_execution_e2e.rs`), which
    runs `worker list` against the same database while `compliance execute` runs.
  - The run's tickets and append-only events reflect in-flight per-file progress
    as work is leased and completed.
- **Recorded per-file breakdown (after the run records its summary):**
  `voom compliance report --job-id <job_id>` returns the run's
  `summary.progress` counts and the per-`(file, phase)` outcomes. The
  `summary.progress` object is:

  ```json
  "progress": { "total": 12, "completed": 9, "failed": 1, "skipped": 2, "remaining": 0 }
  ```

  counted per file by its latest phase outcome: `completed` = committed,
  `failed` = blocked, `skipped` = no work needed / deferred (a distinct bucket,
  not outstanding work), and `remaining` = files not yet in a terminal bucket
  (`0` for a fully-recorded successful run). The same `progress` object is in the
  `execute` command's own output.

  This is a **recorded-run breakdown, not a live ticker**: the workflow summary
  and per-file-phase rows are written once, when the run finalizes (or when a
  partial-failure run finalizes what committed). `report --job-id` therefore
  reports `not found` for a job that is still running and has not yet recorded a
  summary; use the live signals above to watch a run in progress, and
  `report --job-id` to read the recorded breakdown of a run (or a resumed run's
  last recorded summary).

### Cancel queued work

To stop an open job from dispatching more work, run:

```sh
voom job cancel \
  --job-id <job_id> \
  --reason "operator requested stop"
```

Success is the standard single JSON envelope with `command: "job"` and a job
whose `state` is `cancelled`. Missing jobs return `NOT_FOUND`; jobs that are
already succeeded, failed, or cancelled return `CONFLICT`.

Cancellation preserves pending and ready ticket rows as audit evidence, but
those tickets are no longer candidates and cannot acquire a new scheduler
lease. Inspect them with `voom ticket list` and inspect existing leases with
`voom scheduler leases list` or `voom scheduler leases show --lease-id <id>`.

Cancellation does not preempt work that already has a held lease, and there is
no scheduler-lease force-release or worker-abort CLI. A held operation may
finish after the parent job is cancelled. Stop the owning node agent separately when the worker process itself must stop; do not treat
the cancellation envelope as proof that it stopped.

## Output layout

Outputs mirror the source tree. Each terminal artifact lands under
`--output-dir` at the source's path relative to the run's common source root —
a source at `<root>/S01/episode.mkv` promotes to
`--output-dir/S01/episode.…hevc.mkv` (issue #197). Sources sharing a basename
across different subdirectories therefore land at distinct destinations instead
of colliding. The phase-barrier branch IDs are also disambiguated from the
source-relative path for colliding stems (issue #199), so a whole-library run can
include files such as `S01/episode.mkv` and `S02/episode.mkv`. A
single-directory run (no shared subtree) promotes flat, as before.

## Known limitations

The executable primary-audio example is
[`audio-primary-compatibility.voom`](../../crates/voom-policy/fixtures/policies/audio-primary-compatibility.voom).
Its dependent phases retain English and unknown/untagged audio, ensure a compatibility
track, place that track first among audio with the sole audio default, then verify the
artifact. Video, retained originals, commentary, subtitles, and attachments remain present.
Source ranking uses the first English non-commentary original by provider index, with
unknown/untagged originals as fallback. An existing compatible ranked original or associated
companion avoids another encode. Audio placement preserves non-audio positions and order.

The sample requires the existing FFmpeg, ffprobe, and MKVToolNix workers. AAC preserves
mono/stereo. EAC3 preserves supported three-through-six-channel layouts; 7.1 produces
5.1(side). The exact admitted layouts are recorded in [ADR 0111](../adr/0111-primary-audio-compatibility.md).
Missing source facts, no eligible original, unsupported channel/layout pairs, and invalid
companion associations produce explicit compatibility diagnostics rather than guessed
conversions. An ensure failure prevents dependent placement; retention in an earlier phase
may already have committed. Ordinary synthesis keeps its existing non-default behavior.

Install supporting policy, planner, control-plane, agent, probe, and media-worker binaries
before admitting policies with the new operations. Old readers cannot parse the new durable
variants. Before rollback, stop admission of these policies and drain or quarantine affected
work through existing authority; preserve canonical lineage. Reapplication to re-probed
outputs reuses the compatibility identity. This sample does not perform a real-library run
or produce library-wide before/after reports.

- Same-stem, different-extension siblings in one directory can still collide at
  output promotion if their final operation renders the same destination
  basename. Scope the run to one sibling or choose an output directory that does
  not already contain the rendered artifact name.

## Teardown

Close terminal A's stdin or send SIGINT/SIGTERM to the bootstrap helper. It stops
and reaps its agent and API, removes its temporary credential, and records shutdown
results under `bootstrap-evidence`. Keep the database, logs, staging and outputs until
the operator decides retention. A shutdown error is a failed acceptance run.

## run-local stdout contract

Unlike every other `voom` command — which emits exactly one JSON envelope per
invocation — `voom worker run-local` is a long-running foreground supervisor, so
its stdout is a **two-line contract** over the worker's lifetime, in this order
and with nothing else interleaved (all logs go to stderr):

1. A **bare readiness line**, emitted once the bundled worker has bound its
   endpoint and been registered for discovery. It is not wrapped in the standard
   envelope (no `schema_version`/`command`):

   ```
   {"status":"ready","worker_id":12,"kind":"ffmpeg","endpoint":"127.0.0.1:53017"}
   ```

   Wait for this line before dispatching work; gate on `status == "ready"`.

2. The **standard retirement envelope**, emitted once on shutdown (Ctrl-C,
   SIGTERM, or stdin EOF) after the worker row is retired:

   ```
   {"schema_version":"0","command":"worker","status":"ok",
    "data":{"worker_id":12,"kind":"ffmpeg","status":"retired"},...}
   ```

   If retirement fails, line 2 is an error envelope (`status:"error"`) instead.

A consumer can therefore read stdout as: one readiness line, then exactly one
terminating envelope. This contract is enforced end-to-end by
`crates/voom-cli/tests/run_local_stdout_contract.rs` and specified in
`docs/specs/run-local-stdout-contract.md`.

For opt-in PCM16/24 in Matroska, build and configure the paired
[container FFmpeg tools](../../containers/ffmpeg/README.md). The storage-owning
worker runs the containers; source/staging roots stay on that owner.
