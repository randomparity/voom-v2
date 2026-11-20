# Container engine budgets (#897)

## Problem

`containers/ffmpeg/runtime.py` gave every engine command 5 s; loaded runners exceeded it on
`docker create` (run 44615350533) and `docker info` (run 44609059355). The layers above are
fixed numbers below what they wrap: `setup_capture` 20 s, install 60 s, harnesses 90 s,
test-support 10 s (`ffprobe` expired, run 44653479966), PCM test waits, `verify_pcm.py`.

## Scope

`runtime.py` owns the budgets; every outer layer derives from them.
- `SETUP_BUDGET = 30` s: one deadline shared by engine selection (context/machine inspect,
  `getconf`, `info`, image inspect) and `create`; a judgment, as logs show only 5 s exceeded.
- `CLEANUP_BUDGET = 15` s: shared by `rm`/`ps`/`inspect` and reaping the attachment.
- `OPERATION_TIMEOUT` (60 s) = both budgets + `LAUNCH_MARGIN` (15 s: interpreter, `start -a`,
  a short tool). `HARNESS_COMMAND_TIMEOUT` (90 s) adds 30 s for a short media command.
- `check_config_timeout(roots)`: one operation per root plus the margin; install.py's
  `validation_timeout(roots)` adds one operation per installed tool and the margin.

A `Budget` (name, seconds, expiry) reaches `capture`, `Engine`, `remove` and `reconcile`.
Timeouts read `engine command '<engine> <subcommand>' exceeded the <name> budget of <n> s`;
command failures name the command without the `--host` endpoint; a failed absence check keeps
the `rm` failure first. Consumers:
- install.py and `setup_capture` use the derived values; `issue-786-copied-audio-regression.py`
  and `real_owner_audio.py` import `runtime`/`install`: `validation_timeout` for the install
  call, `HARNESS_COMMAND_TIMEOUT` for other commands and the encoder-hiding wrapper;
  `verify_pcm.py` gives each `docker run` that value and its cleanup `inspect` `CLEANUP_BUDGET`.
- `voom-test-support` exports `LAUNCHER_COMMAND_TIMEOUT` (90 s) for `command_output` and
  `flac_command`, and multiples for the commands enclosed: five for worker preflight, eight for
  an audio operation (probes, encode, copied-audio packet scans). Expiry messages name the wait.
- `runtime_integration_test.py` startup/reaping waits allow setup/cleanup plus margin. No
  retries. Rejected: a longer per-command deadline (worst case grows with command count).

### Failure model

- Actors and deployments: local operators (Linux/macOS, Docker/Podman); the GitHub PCM job.
- Invariants: each Scope layer exceeds what it encloses; cleanup removes only the owned
  container; the Rust 90 s equals `HARNESS_COMMAND_TIMEOUT`.
- Accepted failure classes: setup over 30 s fails with the setup message; an unbounded tool or
  witness hash can exhaust a margin; cancelled-create removal can take 45 s; command counts drift.
- Excluded, owned by #907: worker limits below the launcher bounds (ffprobe `-version` 4 s,
  preflight 15 s per command, ffprobe 30 s, startup 5-10 s) and bootstrap `voom()` 60 s. Setup
  slower than those still fails there; a timed-out matroska probe drops PCM WAVEX (stderr only).

## Success
1. Constants are pinned and each outer layer exceeds what it wraps.
2. Timeouts name the command or wait and its budget.
3. The `PCM container (Linux amd64)` job passes on the PR head; no retries or loosened asserts.

## Validation

- Focused-test: `runtime_test.py` pins values, messages, budget sharing, `verify_pcm.py`
  and the Rust constant.
- Focused-test: `install_test.py` pins `validate_install` and `validation_timeout`;
  `launcher_test.rs` pins the 90 s command and the five and eight multiples.
- Task-test-not-applicable (harness and PCM tests): they run only in the PCM CI job.
