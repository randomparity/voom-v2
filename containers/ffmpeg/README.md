# PCM container tools

This optional software profile runs paired FFmpeg/FFprobe 9.0.2 inside a local
Docker or rootless Podman container. It enables `pcm_s16le` and `pcm_s24le` in
Matroska while retaining explicit observed channel layouts. Existing native
`VOOM_FFMPEG_BIN` / `VOOM_FFPROBE_BIN` configurations remain supported.

The image contains a default-off `pcm_wavex` mux option. PCM operations enable
it after strict source precision/rate/layout validation. Its standard
A_MS/ACM WAVEFORMATEXTENSIBLE headers survive ordinary mkvmerge remuxing. No
layout is inferred from channel count, requested settings or tags. PCM16 rejects
24-bit sources; PCM24 preserves 16-bit sources by exact widening. Copied user
metadata, all dispositions and packet bytes/timing remain exact; the generated
Matroska DURATION tag may be recalculated from rounded packet endpoints. Explicit
strict downmix is a separate synthesis operation, retaining original tracks.

## Prerequisites and build

Supported deployments are Linux/amd64 or Linux/arm64 Docker rootful without
userns-remap, Linux rootless Podman, and macOS Docker Desktop or rootless Podman
Desktop. Build the image natively on the selected engine: Apple Silicon uses
Linux/arm64, not an emulated amd64 artifact. Host Python 3.9 or newer provides
stdlib process supervision; Linux requires Python's `os.pidfd_open` and kernel
pidfd support, macOS requires kqueue process notifications. The engine, its
configuration, local image and launcher JSON are operator-controlled trust
boundaries. Arbitrary remote TCP/SSH engines and GPU passthrough are unsupported.

From this checkout, with a running engine and sufficient build storage:

```sh
just container-build docker
# Or: just container-build podman
```

If the build engine cannot reach ffmpeg.org, prefetch exactly
`ffmpeg-9.0.2.tar.xz`, `ffmpeg-9.0.2.tar.xz.asc` and `ffmpeg-devel.asc` from the
pinned official URLs into a private directory, or transfer those same verified
files from a connected build host. Run `just container-build docker /absolute/source-directory`.
No downloads belong in Git. The script rejects missing/extra/symlinked files and
wrong digests before accessing the engine. The builder repeats all digest and
signature/fingerprint checks before extraction. This input transport does not
permit version, URL, signing-key or patch overrides. Debian image/package network
access is still required. Noninteractive Docker Desktop builds may require an
operator-provisioned credential-free client configuration for public registries;
do not disable TLS or unlock a keychain through these scripts.

The command prints the immutable local image ID on stdout and build logs on
stderr. Save that ID in the configuration below. It does not publish an image.
The builder selects the engine's native architecture and uses eight build jobs.
Docker Desktop/Podman machine resources must accommodate that build. Building on
Docker and transferring the same native image into Podman avoids a second build:

```sh
docker image save --output ffmpeg-image.tar IMAGE_ID
podman load --input ffmpeg-image.tar
podman image inspect IMAGE_ID
```

Use the destination engine's inspected immutable ID; image-store IDs can differ.
The recipe pins Debian's multi-platform base digest, signed package snapshots,
codec versions, FFmpeg source/signature/key digests and the signing fingerprint.
It verifies them before compiling. `/usr/local/share/voom-ffmpeg` retains source
provenance, patch, build recipe, package closure, configuration and license.
Inputs are pinned; bit-for-bit reproducibility is not claimed. Updating pins is
a reviewed source change. No host compiler or codec package installation is
performed. The image includes libx264, libx265, SVT-AV1, libaom, AAC, Opus, AC-3,
E-AC-3, FLAC, ALAC and integer PCM, plus the existing MKV/MP4/Ogg profile.

## Grant paths explicitly

Provision canonical, existing, disjoint source and staging directories. Do not
use `/`, symlink aliases, overlapping roots, or paths containing commas or
newlines. Relative media arguments resolve from the worker's current directory,
which must be inside one grant. The same absolute paths exist in the container.
Grant source roots read-only and staging roots read-write; no path is guessed
from FFmpeg arguments. A staging root should be writable by the worker's UID/GID.

Create an operator-owned JSON file, using actual canonical paths and image ID:

```json
{
  "engine": "docker",
  "image": "sha256:<64 lowercase hexadecimal digits>",
  "read_only_roots": ["/srv/media"],
  "read_write_roots": ["/srv/staging"]
}
```

On macOS also set `"desktop": "desktop-linux"` for the existing Docker Desktop
context, or the exact existing Podman machine name. Linux rejects `desktop`.
The launcher inspects and freezes that local Unix-socket endpoint. Unset
DOCKER_HOST, DOCKER_CONTEXT, DOCKER_TLS_VERIFY, DOCKER_CERT_PATH, CONTAINER_HOST
and CONTAINER_CONNECTION; these conflict with explicit endpoint selection.
No engine/default/context is changed automatically. Podman Desktop must expose its
forwarded socket under the native macOS user temporary directory. The launcher
uses `getconf DARWIN_USER_TEMP_DIR` only for machine inspection, then freezes
the verified socket. Worker/media TMPDIR remains unchanged; Podman otherwise
derives a nonexistent socket from that staging path. Start the named machine
with its normal Desktop environment if the native socket is absent.

Docker Desktop must share the configured host roots. Podman Desktop requires
operator-provisioned **same-path** VM shares, for example when creating a new
machine with `podman machine init --rootful=false --volume HOST:HOST NAME`.
Supply each intended share explicitly; inspect installed CLI help for defaults.
Do not alter another workload's machine or assume Docker's sharing configuration
applies to Podman. A missing share or differently mapped VM directory is a setup
failure. Public Podman machine inspection does not expose its mounts; this code
does not read internal Podman configuration. Trusted operator provisioning is the
mapping authority, and the setup witness below verifies access/content.

On enforcing SELinux, roots must already have labels permitting container access.
Provision dedicated container-accessible storage under the operator's existing
policy. These tools never relabel/chown existing roots or media, disable SELinux
separation, use `:U`, grant privilege or mount the engine socket. Unprepared
Podman roots fail with a setup diagnostic. For tests only, deliberately provision
labels on newly generated private fixture roots or a Podman-managed shared-label
volume; do not relabel an existing library to make a test pass.

## Verify and opt in

Run the explicit access check before enabling the launchers. Provide one existing
regular, nonempty, canonical source witness per read-only root. Choose small
files: setup compares their complete SHA256 without changing them. It also
creates/removes private staging nonce files and checks actual host UID/GID.
Docker runs with the host UID/GID; Podman uses
`--userns=keep-id:uid=65532,gid=65532` with `--user 65532:65532`, mapping its server
user to the fixed nonroot image identity. Plain keep-id may retain the image USER
without mapping it to the server user on some versions. Desktop mappings must
produce files owned by the invoking host user;
the check fails rather than chowning unexpected output.

```sh
export VOOM_MEDIA_CONTAINER_CONFIG=/absolute/path/container.json
cd /srv/staging
python3 /absolute/checkout/containers/ffmpeg/runtime.py check-config \
  --source-witness /srv/media/existing-small-file
export VOOM_FFMPEG_BIN=/absolute/checkout/containers/ffmpeg/ffmpeg-container
export VOOM_FFPROBE_BIN=/absolute/checkout/containers/ffmpeg/ffprobe-container
```

Start the existing worker command with that environment and canonical cwd/TMPDIR.
Do not select NVIDIA, VAAPI or VideoToolbox device environment settings with this
CPU-only profile. Missing engine, local image, encoder or PCM mux capability fails
before output staging; there is no pull, build, fallback or startup requirement
for unrelated native PCM-unavailable workers. The source witness proves selected
content/access, not unique filesystem identity. Recheck after changing shares.

Worker subprocess helpers set their own `VOOM_MEDIA_CALLER_PID` before spawning.
For direct shell diagnostics, use an ordinary child invocation:

```sh
VOOM_MEDIA_CALLER_PID=$$ "$VOOM_FFMPEG_BIN" -version
```

Do not `exec` the launcher or inherit another process's PID. Missing/wrong caller
identity fails before container creation. Binary stdin, stdout/stderr, exit status,
relative paths and paths with spaces retain their normal meanings.

## Cancellation and recovery

An operation-only guardian monitors a lease held by the launcher. The launcher
monitors its verified invoking process using pidfd/kqueue. Caller death,
launcher TERM/KILL and worker timeout close the lease; the guardian removes only
its exact owned container and reaps its attachment process. Normal exit also
removes the container before returning the tool's exit status. No pruning or
prefix-based removal occurs; unrelated containers remain untouched.

Each setup/create RPC has a five-second deadline. Cleanup has a total five-second
budget; cancellation during create can require roughly ten seconds of attempts.
This requires a responding engine and surviving guardian. Engine outages, daemon
creation committed after the RPC deadline, or killing the guardian/whole cgroup
cannot guarantee removal. A failure reports the exact owned operation/container
identifier and returns nonzero when reporting remains possible. Investigate that
identifier on the **same configured endpoint**, verify its `io.voom.media-operation`
label, then remove that exact container after restoring the engine. Never use a
blanket prune as recovery. There is no persistent recovery service.

## Verification

```sh
just setup-workflow-tools  # pinned source-check analyzers
just container-unit-check
just container-check docker IMAGE_ID
just container-runtime-check docker IMAGE_ID
just container-worker-check docker IMAGE_ID
```

The first command needs no engine. The other commands require the built image;
missing prerequisites are failures. `container-check` additionally uses host
mkvmerge to inspect and remux headers. It checks 20 PCM cells, integer samples,
normal remuxes/default-off controls, copied audio packet timing and the software
encoder profile. Opt-in muxing stores observed positive audio BlockDuration
values so remuxing cannot discard their rounding; absent durations are not invented. Video profile
proof writes seekable MKV files: upstream libaom's deferred AV1 extradata cannot
be finalized in a Matroska pipe, while the worker writes regular staging files.

Desktop and dedicated-root variants accept `--desktop NAME` and `--root PATH`
on `runtime_integration_test.py` and `worker_check.py`. Invoke those Python scripts
from this directory or by absolute path. The root must be newly owned test storage
already shared/labeled for the selected engine. Tests remove their generated
fixtures and exact containers; image/VM installation and removal remain operator
operations. Native suites explicitly ignore four named PCM container cases;
the required Linux Docker CI job executes all four. Hosted native macOS CI does
not claim Docker/Podman Desktop coverage; those arms require actual Desktop hosts.
