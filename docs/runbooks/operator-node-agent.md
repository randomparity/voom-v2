# Runbook: Pull-Based Node Agent

Run one `voom-node-agent` process on each remote worker host. The agent authenticates as
one pre-registered logical node, pulls leases from `voom-api`, and supervises the configured
out-of-process workers over loopback HTTP.

## Register the node and protect its token

On the control-plane host, initialize the database and register a remote node once:

```sh
export VOOM_DATABASE_URL=sqlite:///var/lib/voom/voom.db
voom node register --name media-node-1 --kind remote --heartbeat-ttl-seconds 30
```

The successful JSON envelope contains the node ID and the bearer token. The token is shown
only at registration. Transfer it through the deployment secret store, write it as a
mode-0600 file owned by the agent service account, and do not place it in process arguments,
logs, or the TOML document itself.

```sh
sudo install -o voom -g voom -m 0600 node-token /etc/voom/node-token
```

## Configure the agent

The configuration is strict TOML: unknown or duplicate fields, invalid bounds, relative
worker programs, duplicate worker names/capabilities, and empty capability lists stop
startup. This example reads the token from a file:

```toml
control_plane_url = "https://api.example.test:7443"
ca_cert = "/etc/voom/client-ca.pem"
node_id = 7
poll_interval_ms = 250
lease_ttl_seconds = 30
progress_idle_timeout_seconds = 120
shutdown_grace_seconds = 10
node_token = { source = "file", path = "/etc/voom/node-token" }

[[workers]]
name = "probe"
program = "/usr/local/libexec/voom/voom-ffprobe-worker"
args = []
operations = ["probe_file"]
artifact_access = ["shared_mount"]
max_parallel = 2
```

An accelerator-bound `transcode_video` worker adds one tagged descriptor beneath
that worker. The descriptor pins the exact startup-probe result that activation
stores in `worker_capabilities.hardware` and `extra.accelerator`; identity is a
full GPU UUID, lowercase PCI address, or host resource ID, never a device
ordinal or render-node path:

```toml
[[workers]]
name = "ffmpeg-vaapi"
program = "/usr/local/libexec/voom/voom-ffmpeg-worker"
args = []
operations = ["transcode_video"]
artifact_access = ["shared_mount"]
max_parallel = 2

[workers.accelerator]
backend = "vaapi"
pci_address = "0000:f4:00.0"
device_name = "Radeon Pro"
driver_version = "Mesa 26.1"
encoders = ["hevc_vaapi"]
decoders = ["hevc"]
max_sessions = 2
```

The same tagged contract accepts `nvidia` and `video_toolbox` descriptors with
their backend-specific identity and probe results. The agent passes only the
stable identity and `max_sessions` to the child. The child probes that device
and returns structured readiness metadata; every descriptor field must match
the configuration before the agent reports ready. Unknown fields, identity/token
mismatches, unstable identities, and session capacity outside `1..=16` fail
configuration, activation, or child startup.

An environment-backed secret is also supported:

```toml
node_token = { source = "env", name = "VOOM_NODE_TOKEN" }
```

Use the environment form only when the service manager injects secrets without exposing
them in unit files or diagnostics. The agent trims one trailing newline from either source
and rejects embedded newlines.

Remote control-plane URLs must use HTTPS. `ca_cert` adds a private/custom CA while retaining
certificate and hostname verification. Cleartext HTTP is accepted only for an explicit
loopback host, for local testing; there is no insecure remote override.

## Start and supervise

Validate the installed binary version and start the foreground process under a supervisor:

```sh
voom-node-agent --version
voom-node-agent --config /etc/voom/node-agent.toml
```

Startup creates a fresh random incarnation and activates the declared workers
as not ready. It sends the first node heartbeat while children start. Only
after every child binds, returns matching accelerator metadata when configured,
completes the exact-version handshake, proves its identity, and finishes
dependency preflight does the agent persist readiness and begin lease
acquisition. The control plane accepts readiness only for the authenticated
node's current incarnation and worker while its heartbeat is fresh. A child
crash persists not-ready before lease settlement and restart, then persists
ready after the replacement repeats every startup proof and before acquisition
resumes. Starting a second agent for the same node atomically supersedes the
first incarnation. The superseded process is fenced from heartbeat, readiness,
acquire, and terminal mutations and exits unsuccessfully; do not configure two
instances as an availability pair.

Node heartbeats run independently from child startup and lease dispatch. Every held lease
also has its own heartbeat, which stays active until completion/failure is acknowledged.
`progress_idle_timeout_seconds` starts when a worker is dispatched and resets only after a
valid NDJSON progress frame. Silence, malformed frames, a child exit, or a protocol failure
settles the lease with the corresponding failure class.

After a child exits, the agent first persists not-ready, then cancels and
settles that child's held leases before attempting a restart. Three consecutive
startup failures exhaust the restart budget, retire the incarnation with
`child_restart_exhausted`, and make the agent exit unsuccessfully. A child that
starts cleanly and then crashes is bounded separately: more than three crashes
within sixty seconds exhausts the same budget, so a worker that dies on every
dispatch cannot respawn indefinitely. Let the service supervisor apply its
normal process-level restart/backoff policy.

## Behavior when the control plane is unreachable

The agent does not keep working against a control plane it cannot reach. If no lease
heartbeat succeeds within `lease_ttl_seconds`, it fences that lease locally and stops the
dispatch rather than racing the control plane's redispatch of the same ticket. If no node
heartbeat succeeds within the incarnation TTL, the agent treats the incarnation as lost and
exits unsuccessfully. Both deadlines match the ones the control plane applies, so a
partition ends with the node idle rather than executing work a second node has taken over.

## Inspect operation

Run inspection commands on the control-plane host against the same database:

```sh
export VOOM_DATABASE_URL=sqlite:///var/lib/voom/voom.db
voom node show --node-id 7
voom node incarnation list --node-id 7 --limit 20
voom worker list --status active
voom worker list --status retired
voom scheduler decisions list --limit 20
```

The incarnation list is newest first. A normal replacement shows the new incarnation as
`active` and the prior one as `superseded`; a normal stop records `retired` with
`graceful_shutdown` and retires its workers.

## Stop and upgrade

SIGINT or SIGTERM stops acquisition, fails held leases as `user_cancellation`, closes and
reaps every child (killing one after `shutdown_grace_seconds` if necessary), and deactivates
the incarnation. Set the supervisor stop timeout above the configured shutdown grace. The
first operator signal always begins or acknowledges ordered shutdown, even when an internal
fatal error or restart-budget exhaustion already began it; that unconsumed first signal never
forces settlement or deactivation. Only a genuine second operator signal, after the first has
been consumed, cancels blocked lease settlement or deactivation and makes the process exit
unsuccessfully. It still kills and reaps every child before exit, but forced shutdown can leave
the incarnation or lease terminal state for TTL expiry/recovery to reconcile.

A signal that arrives while the agent is still retrying activation against an unreachable
control plane stops it immediately and exits successfully. No child has started at that
point; if an earlier activation attempt did land, heartbeat expiry reconciles it.

The schema change for this release is a pre-release flag day, not a rolling mixed-version
upgrade:

1. Stop the agents and API, then take the WAL-aware SQLite backup described in
   [migration-rollback.md](migration-rollback.md).
2. Install the matching `voom`, `voom-api`, `voom-node-agent`, and worker binaries on every
   host before changing the database.
3. Run `voom init` once with the new CLI to apply migrations.
4. Start `voom-api`, verify `/health`, then start each node agent and inspect its incarnation
   and workers.

Do not run old binaries against the migrated database. Rollback is paired: stop the new
processes, restore both the prior binaries and the pre-upgrade database backup, then start
the prior deployment. Restoring only one side is unsupported.

## Current boundary

This agent supplies authenticated pull execution, fencing, heartbeats, child supervision,
and explicit artifact-access-plan forwarding. Issues #418 through #425 remain future
byte-local work: node-owned root persistence and resolution, scan/hash/probe conversion,
transform and backup conversion, storage-owner commit, and removal of transitional
control-plane filesystem access are not available through this agent yet. Do not infer
owner-local path resolution or byte transfer from `shared_mount` declarations.
