# PCM archival audio and an opt-in container runtime — issue #763

## Authority and scope

Frozen charter: issue #763, token `q763-8c9d96fa`, WORK:SCOPE
[6482597768](https://github.com/randomparity/voom-v2/issues/763#issuecomment-6482597768).
The operator explicitly authorized building custom FFmpeg in Docker/Podman and
running FFmpeg in that container. This is one coupled release unit: the two PCM
targets cannot satisfy ADR 0114 without the mux capability, and that capability
must have an installable runtime. No independent product is being absorbed.
[ADR 0117](../adr/0117-container-pcm-muxing.md) records the toolchain boundary.

## Global Constraints

Rust 1.95.0; MKV only; protocol version 5 unchanged. Native executable selection
and existing codecs remain supported. The opt-in runtime supports Linux Docker
rootful without userns-remap, Linux rootless Podman, macOS Docker Desktop and
macOS Podman Desktop with its local machine. Native Linux/amd64 and Linux/arm64
images cover Intel and Apple Silicon Desktop engines; no emulation assumption.
Host Python 3 with fork, non-inheritable pipes and a process-exit watcher is an
explicit launcher prerequisite: Linux pidfd or macOS kqueue NOTE_EXIT. Check
capabilities rather than inventing a Python or project-wide version floor.
Build FFmpeg 9.0.2 from verified source in pinned Debian 13.7-slim. Build and use
images locally and in CI; no registry publication or upstream PR is authorized.
No privileged containers, host-root mounts, automatic dependency installation,
source relabeling, SELinux-disable option, internal EBML rewriter, or fallback.
Byte operations remain in the owner worker/node; control-plane authority and
transaction/publication boundaries remain unchanged. Arbitrary remote engines,
GPU passthrough and Linux rootless Docker remain outside this runtime profile.

## PCM behavior

Add canonical `pcm_s16le` and `pcm_s24le` to the existing closed audio target
sets, optional encoder inventory, deterministic argv and verification paths.
Target spelling defines integer precision. An absent lossless precision setting
uses that target precision; a present setting must match it. Resolve each source
with ADR 0114 before any output command. PCM16 rejects a 24-bit source; PCM24
accepts 16-bit widening. No bitrate or new profile knob is introduced.

Ordinary archival accepts known signed 16/24-bit source precision, s16/s16p or
s32/s32p decoder storage, positive rate, and the exact existing layouts mono,
stereo, 5.1, 5.1(side), 7.1. Preserve rate/layout and representable samples.
Unknown, floating, contradictory or unsupported source facts fail loudly before
output creation, including a later invalid selected stream. Container/format,
profile, channel and precision combinations retain existing strict validation.

Explicit synthesis retains originals and adds a non-default companion with
strictly fewer channels, using existing mono/stereo/back-5.1 target layouts.
Resolve source precision before the requested downmix. This is a transform;
compare it with an independent reference downmix, not an archival equality claim.
Preserve metadata, full accessibility dispositions, identities and lineage.

Emit s16 for PCM16 and s32 with raw precision 24 for PCM24, with explicit rate
and layout. Add `-pcm_wavex 1` to PCM MKV commands. After encoding, verify actual
canonical codec, rate, layout, channels and precision using the existing strict
facts helper. Never infer layout from count, the request, or a metadata tag.
Decoded signed-s32 equality is required generated-media proof; it does not add
production full-media reads. Existing source hashes, no-overwrite, unaffected
packet/timestamp checks and artifact publication guards retain their ownership.

## FFmpeg representation

FFmpeg 9.0.2 still routes these codecs to A_PCM/INT/LIT, which omits a usable
explicit layout in observed probes. Standard A_MS/ACM with a 40-byte
WAVEFORMATEXTENSIBLE header carries the layout mask and significant precision.
Earlier 8.1.3 fixture proofs and normal MKVToolNix 99 remuxes retained observed
facts and sample equality for ten depth/layout combinations; the new build must
independently prove the same output. Fixture evidence is not product support.

The scoped patch changes matroskaenc.c, riff.h and riffenc.c only. Add boolean
Matroska mux option `pcm_wavex`, default false. When enabled, select ACM only for
non-WebM PCM_S16LE/PCM_S24LE with a valid native layout and nonzero mask below
0x40000. Compute this selection once and pass it to codec-private assembly.
Bypass native codec-ID lookup for that track, reuse existing ACM/WAV writing,
and add internal flag FF_PUT_WAV_HEADER_FORCE_WAVEFORMATEXTENSIBLE (bit 0x4).
OR that flag into the existing extensible-header condition. Force WAVEX even
for 16-bit mono/stereo: FORCE_WAVEFORMATEX alone does not preserve their masks.
No packet encoding, global codec table or demuxer change is needed.

Headers carry tag 0xfffe, cbSize 22, valid bits 16/24 and the PCM GUID. Expected
masks: mono 0x4, stereo 0x3, back-5.1 0x3f, side-5.1 0x60f, 7.1 0x63f.
Default-off output and other codecs keep upstream routing. With the option on,
unknown/unrepresentable copied PCM tracks also retain upstream routing; this
preserves unaffected tracks without inventing their layout. Known-layout copied
PCM16/24 tracks may gain WAVEX headers, with their packet bytes/facts preserved.
Selected PCM outputs never use that exception: strict source/output validation
must succeed. Tests include mixed selected and untouched unknown-layout PCM.

## Build and provenance

Use a two-stage image, with FFmpeg libraries linked into the executables and
Debian codec runtime libraries installed in the final stage. Keep FFprobe paired
with the exact FFmpeg build. No host codec/compiler installation is necessary.

Pinned inputs:

- FFmpeg 9.0.2 archive: `https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz`, SHA256
  `8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e`.
- Detached signature SHA256:
  `d617fd94ea354dadd2a8bb16243d37c44e1e9729b06b6ad58fe30bfb0fa943f2`.
- Official ffmpeg-devel.asc SHA256:
  `397b3becedcd5a98769967ff1ff8501ddc89f8368b8f766e4701377d7dbaabe5`;
  fingerprint `FCF986EA15E6E293A5644F10B4322F04D67658D8`.
- `docker.io/library/debian:13.7-slim@sha256:f6b08b09a05725f9431502d3b5714f5c4576e82c1a84fa1debe0c24a7e0c3ec4`.
- Native base children: amd64
  `sha256:1e1a22175fbf1b32157ddaf0219e1db4e5401fab2b94ed285add64d18587cd2d`;
  arm64/v8 `sha256:1470991b497d26788dc4d6f438bf5690c6ba0d499235d237e55f4b43caea7a75`.
- Debian and debian-security snapshots at `20261027T000000Z`, suites trixie and
  trixie-security, main only. Replace floating sources in both stages. Retain
  signed apt verification; disable Valid-Until only for these frozen snapshots.
  Bootstrap CA certificates through signed apt at the HTTP snapshot, then HTTPS.

Pin codec development/runtime versions: x264 2:0.164.3108+git31e19f9-2+b1;
x265 4.1-2; SVT-AV1 2.3.0+dfsg-1; Opus 1.5.2-2; aom 3.12.1-1+deb13u1;
zlib 1:1.3.dfsg+really1.3.1-1+b1. Snapshot package closure is recorded with
`dpkg-query` for both stages and both native architectures. Frozen arm64 and
amd64 codec versions match, so no architecture-specific pin fallback is needed. Verify archive, key and signature before extraction,
verify the exact fingerprint, and apply the patch without fuzz. Record patch
hash, configure command, source pins, package manifests and license information
inside the image. Inputs are pinned and verifiable; bit-for-bit build identity
is not promised. Record final local immutable image ID, target architecture, build time and size.
Select the native engine architecture explicitly (amd64 or arm64); validate the
pinned base index child and package closure for that architecture. Build on the
corresponding engine, including an actual Apple Silicon Desktop arm64 build.
Do not silently run an amd64 image under emulation on an arm64 Mac.

Configure GPL, libx264, libx265, libsvtav1, libopus, libaom, zlib and iconv;
disable autodetection, network protocols, ffplay, docs, debug and shared FFmpeg
libraries. Retain default codecs, formats, filters and avdevice/lavfi. Existing
fixtures require libx264 plus lavfi testsrc. Keep portable CPU defaults. Verify
configure symbols, final dynamic-library closure, paired versions, encoders,
MKV/MP4/Ogg muxers and PCM option in the final image. Bound make parallelism to
8 by default and verify disk/RAM/architecture before the first full build.

## Launcher and deployment interface

Two executable entry points, `ffmpeg-container` and `ffprobe-container`, select
fixed `/usr/local/bin/ffmpeg` and `/usr/local/bin/ffprobe` inside the image.
Operators opt in with existing VOOM_FFMPEG_BIN and VOOM_FFPROBE_BIN. Both use one
shared standard-library Python launcher; no worker transport or runtime enum.
VOOM_MEDIA_CONTAINER_CONFIG names an absolute operator-owned JSON file:

```json
{
  "engine": "docker",
  "image": "sha256:<64 lowercase hexadecimal digits>",
  "read_only_roots": ["/srv/media"],
  "read_write_roots": ["/srv/staging"]
}
```

Accept exactly docker or podman, an immutable local image ID or repository
digest, and canonical existing directory roots. On macOS additionally require
`desktop`: the explicit Docker context name or Podman machine name; reject that
field on native Linux. This selects an existing deployment and never creates it. Reject unknown fields, wrong
shapes, duplicate/overlapping roots, symlink roots, root `/`, and characters the
engine bind grammar cannot represent safely. Resolve argv structurally, never
through a shell. Roots are explicit authority, not inferred from FFmpeg argv.
Bind at identical absolute paths; require cwd inside a declared root so relative
and absolute paths keep their meaning. Mount RO roots read-only and staging RW.
Operators place the worker cwd within one configured root. Tests run from an
owned directory under the repository's pinned temp root, not an arbitrary /tmp.

On Linux, support local Docker rootful without userns-remap and local rootless
Podman. On macOS, inspect the explicitly named Docker Desktop context or Podman machine.
Verify Docker's local Unix socket endpoint and actual engine mode; freeze that
socket in every command. For Podman, require the named machine's State=running,
Rootful=false and ConnectionInfo.PodmanSocket.Path identifying an existing local
Unix socket; use the inspected socket through explicit `podman --url unix://...`
for every command. The installed client/server must support that forwarded
Desktop socket or fail with a prerequisite diagnostic; do not silently substitute
an arbitrary SSH service. Reject remote endpoints and conflicting steering.
Linux Podman remains explicitly local. No Docker Desktop installation, machine
creation, engine switching or root provisioning occurs automatically.

Docker runs with the caller's numeric UID/GID. Podman uses --userns=keep-id;
on macOS that preserves the VM/server user, not necessarily the Mac UID. Do not
force the Mac UID into a rootless VM namespace. Verify actual host output UID/GID
through a newly owned staging probe on each engine; fail if ownership differs
from the invoking host user. Prove source reads and directory/write permissions
through the configured shared paths. Do not compensate with chown, :U, elevated
groups or root execution. Desktop filesystem sharing is an operator prerequisite;
missing shares fail actionably, never copy media to a new byte owner. For Podman,
operators provision same-path shares with documented machine-init --volume
HOST:HOST. Public machine-inspect has no mount table; do not parse internal
Podman storage. Docker Desktop transparently maps granted host bind paths through
its VM. Trusted operator provisioning is the mapping authority.

Before enabling a configuration, run the explicit check-config command with one
--source-witness existing regular nonempty file per RO root. It compares complete
SHA256 at the same absolute path on host and container without writing the source.
Choose a small witness; hashing cost is explicit setup work, not a new per-media
read. Missing witnesses, empty/inaccessible files or differing contents fail with
an actionable setup diagnostic. An owned random staging probe must round-trip
host→container→host bytes and host UID/GID, then be removed. This proves access
and selected content equality, not unique filesystem identity. No source witness
is created automatically. Per-invocation root access checks do not replace this
operator setup prerequisite or claim to prove a unique backing directory. Require canonical paths
consistently for roots, cwd, requests and TMPDIR; undeclared macOS /tmp or /var
aliases fail actionably rather than rewriting arbitrary FFmpeg arguments.

Use --pull=never, read-only image root, network none, cap-drop ALL and
no-new-privileges. No devices, engine socket, host PID or privileged mounts.
Verify image presence/platform and selected engine before creating an operation.
No runtime pull or build. CPU-only container configuration rejects selected
hardware acceleration before staging. Native hardware support remains intact.

Media roots must already be container-accessible. Setup verifies actual reads
and owned temporary staging writes/cleanup through the selected image; permission
bits alone do not prove SELinux access. Never relabel or chown an existing root
or source. Podman on enforcing SELinux fails actionably for unprepared roots;
document operator provisioning. Tests may label only newly generated owned
fixtures or use an already shared-labelled private volume. No label-disable knob.

## Operation lifecycle

A signal trap or `exec docker run --rm` is insufficient: both tested engines
left containers running after the CLI was killed. An operation-only guardian
owns the complete create/start/remove sequence. No persistent service is added.

The invoking Rust process sets VOOM_MEDIA_CALLER_PID to its own process ID on
its Command immediately before spawning. Do this for FFmpeg's common async output
helper and synchronous preflight command helper, plus FFprobe's async command
helper and synchronous version check. Do not mutate the global environment.
Native binaries ignore this private execution-boundary value. Direct launcher
callers and test tools must set their own PID before spawn; missing or inherited
wrong identity fails before any container is created. Shell usage is an ordinary
child invocation with `VOOM_MEDIA_CALLER_PID=$$`, not `exec` replacing that shell.

The launcher requires actual parent==expected caller, registers a process-exit
watch, and rechecks parent equality plus an already-dead event before releasing
creation. macOS uses EV_ADD|EV_ONESHOT and checks EV_ERROR/registration exceptions;
there is no success path after failed process-watch registration. Linux uses pidfd; macOS registers kqueue EVFILT_PROC/NOTE_EXIT and fails
closed on ESRCH/registration error or pending exit. This closes the reproduced
race where an already orphaned launcher would otherwise bind init/a subreaper.
Keep the process watcher in the launcher: macOS kqueue is not inherited by fork.
The launcher continuously monitors caller exit while waiting for its guardian.
On caller death it closes the guardian lease and exits; it never guesses liveness
from elapsed time or bare PID existence.

Create a non-inheritable pipe before forking the guardian into its own session.
Only the launcher retains the write end; the guardian closes it and watches read
EOF. Engine subprocesses inherit neither lease descriptor. Before creation,
check the lease. Create a uniquely named and operation-labelled container under
a bounded command, record the full returned ID, recheck liveness, and start it
attached with stdin enabled and no TTY. Pass stdout/stderr directly to the caller;
no undrained intermediary pipes. The separate lease preserves binary stdin,
including immediate input EOF, without a framing protocol or in-image helper.

Watch attachment exit and lease EOF concurrently. On normal exit, remove the
owned container then preserve its exit code. On cancellation or caller death,
force-remove the exact ID, terminate/reap the attachment and close stream handles.
Docker uses rm -f; Podman uses rm -f --time 0. Never prune or remove by prefix.
Use the unique name only to reconcile a create whose response was interrupted;
verify its operation label before removal. Do not touch a foreign container.

Setup/create commands have 5-second deadlines. Cleanup has a total 5-second
budget including removal and attachment reaping; liveness poll interval is
50 ms. A cancellation during create may consume the remaining create deadline
before cleanup, so cleanup attempts finish within roughly 10 seconds while the
engine responds. Failed cleanup returns nonzero and reports the exact owned
container identifier on stderr for operator recovery. Distinguish proven absent
from daemon/permission/parse errors. Never convert an engine error to success.

This mechanism covers launcher SIGKILL, ordinary worker death and process-group
termination while the guardian and local engine remain available. It cannot
promise removal if the engine is unavailable, commits a create after its RPC
deadline, or the guardian itself/its whole cgroup is killed. Those infrastructure
failures remain visible limits, not successful cancellation claims. A framed
stdin lease with an in-container supervisor could strengthen the boundary but
adds buffering, framing and media-stdin semantics that this contract does not
require. Host FIFOs are not assumed to cross a Desktop VM filesystem boundary.

## Readiness and failure ordering

Retain standard native preflight. Discover the exact `pcm_wavex <boolean>`
AVOption row from successful Matroska muxer help, with a false default in manually
constructed worker configuration. Missing option is an optional capability,
not a new native startup requirement; failed help execution is a diagnostic,
not proof of support. PCM encoders are optional inventory tokens.

Handler order: validate target/settings/format, require requested encoder and
PCM mux capability, validate paths/source, probe/resolve every selected stream,
then launch output creation. Existing codec requests acquire no PCM requirement.
The launcher itself fails before engine creation when config, engine, local mode,
image or mounts are invalid. Setup/readiness verifies the paired tools and
mount access; each launch still checks prerequisites that can disappear.
Failed input facts or capability never produce output staging bytes. Normal
operation-scoped containers and accessibility-check scratch are not artifacts.

## Verification and rollout

Focused Rust tests cover policy/protocol/planning/control-plane binding, exact
argv without bitrate, option/readiness parsing, default-false capability,
precision conflicts/reduction, malformed/missing facts, unsupported formats,
selection atomicity and strict output mismatch rejection. Controlled mutations
must demonstrate that new precision/capability/layout tests fail when weakened.

Actual patched-container tests cover 16/24 bits × five layouts × 44.1/96 kHz,
canonical observed codec/precision/layout and decoded equality; 16-to-24 widening;
separate strict downmix/reference; multiple selected streams; unaffected packet
hashes/timestamps/metadata/accessibility/dispositions/lineage and unchanged source.
Inspect 40-byte CodecPrivate and remux through installed MKVToolNix. Include
untouched unknown-layout PCM and default-off/native patch behavior.

Launcher unit/fake-engine tests cover invalid config/roots/endpoint/image,
stdin/argv/exit status, chatty streams, missing prerequisites, delayed create,
late response, cleanup failure, parent-death race, macOS watch registration races and lease-FD inheritance.
A deterministic fixture delays launcher initialization until after worker exit
under a live subreaper; expected caller identity must reject that adoption.
Exercise both synchronous preflight and async FFmpeg/FFprobe launch seams.
Real Linux and macOS Desktop Docker/Podman tests cover normal exit, cancellation before/after create,
worker timeout, launcher SIGKILL and caller death, plus foreign-container
preservation, RO enforcement, relative paths, spaces, output UID/GID, and
unprepared-root failures and missing/empty/mismatched source witnesses. Repeat races; disclose first-run findings.

Native Linux/macOS CI remains. Add a required Linux container job that builds
the pinned image, runs launcher lifecycle and explicit PCM conformance tests.
Transfer a verified native-architecture image to the other engine on the same
host to avoid a duplicate source build. Run native arm64 image build/media proof
on Apple Silicon; do not treat Linux amd64 proof as macOS or arm64 proof.
macOS Desktop execution requires a prepared reachable Mac with both engines;
GitHub-hosted arm64 macOS runners do not support nested virtualization. Keep
portable source/unit checks in existing macOS CI; actual Desktop engine/media
proof runs on the prepared Mac and records its exact candidate head. A missing
Mac proof remains a completion blocker, not a skip or an unsupported-platform
claim. No automatic self-hosted runner registration is authorized.
Container media tests are explicitly isolated from native test arms, with named
ignore reasons and an explicit required CI invocation; no silent environment
skip. Report exactly which engine/OS/architecture arms ran. Source guards validate
the new scripts/workflow. Run focused checks, source-checks, fmt-check, lint,
mandatory prek pre-commit, explicit final just ci before push, and hosted CI.
Deploy by local build and explicit launcher configuration; rollback selects the
original native executable paths and leaves PCM requests failing closed.

## Failure model

- Actors/deployments: authenticated owner workers and trusted local operators;
  untrusted media; trusted locally configured engine/image/config; Linux and macOS Desktop container proof using native amd64/arm64 images,
  alongside existing native Linux/macOS execution arms.
- Assets/invariants: source bytes and labels, integer sample facts, copied media,
  original/non-default identities, owner-node byte boundary, explicit mount
  grants, output ownership, no encoder after bounded cancellation under the
  stated functioning-engine/guardian conditions.
- Accepted failure classes: daemon/host/guardian failure can prevent removal;
  stderr may be lost when the caller has died. Failures never authorize success,
  publication or precision/layout guessing. Cross-build encoded bytes are not
  promised identical. Unknown copied-track layout remains unknown.
- Covered elsewhere: #760 foundational facts, #759/#761/#762 other codecs,
  #764 aggregate CLI matrix, #339 real libraries, #758 wider PCM/exports/upmix,
  #765 ranking/defaults, ADR 0050 byte-ownership migration. No new deferral issue.

## Threat model

- Added boundaries: operator config into local engine argv; immutable source and
  base downloads into image builds; worker process lifetime into container lifetime;
  declared host roots into container mounts. Existing media/probe boundary widens
  to PCM. No new network listener, control-plane byte path or persisted schema.
- Actors: policy/media may contain hostile facts and names; local operators
  choosing engine/config/image are trusted with their existing filesystem rights.
  A compromised privileged engine, image or operator is outside this containment
  promise; the launcher is not a sandbox against those trusted components.
- Controls: strict JSON/root/image validation, fixed entrypoints, no shell/eval,
  local-mode checks, least mount/UID authority, signed/pinned downloads, no network
  or privilege in runtime, bounded subprocesses, exact-owned removal, source hashes
  and strict observed facts. Error text contains actionable paths only locally;
  public workflow artifacts omit host/user identifiers.
- Outside scope: defending against host-root/daemon compromise or whole-host loss,
  arbitrary remote filesystems, GPU/device delegation, registry distribution,
  persistent orphan recovery services and new transport authentication.

## Primary evidence

[FFmpeg releases](https://ffmpeg.org/download.html),
[9.0.2 muxer](https://github.com/FFmpeg/FFmpeg/blob/n9.0.2/libavformat/matroskaenc.c),
[WAV writer](https://github.com/FFmpeg/FFmpeg/blob/n9.0.2/libavformat/riffenc.c),
[Matroska codecs](https://www.matroska.org/technical/codec_specs.html),
[WAVEFORMATEXTENSIBLE](https://learn.microsoft.com/en-us/windows/win32/api/mmreg/ns-mmreg-waveformatextensible),
[Debian snapshots](https://snapshot.debian.org/),
[Docker run](https://docs.docker.com/reference/cli/docker/container/run/),
[Podman run](https://docs.podman.io/en/latest/markdown/podman-run.1.html),
[Apple kqueue](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/kqueue.2.html),
[Podman machine inspection](https://docs.podman.io/en/latest/markdown/podman-machine-inspect.1.html),
[Podman on macOS](https://podman.io/docs/installation),
[Docker Desktop permissions](https://docs.docker.com/desktop/setup/install/mac-permission-requirements/),
[GitHub runner limitations](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
