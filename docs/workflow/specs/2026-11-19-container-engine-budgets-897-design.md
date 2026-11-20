# Container engine budgets (#897)

## Problem

`containers/ffmpeg/runtime.py` gave every engine command 5 s; loaded runners exceeded it on
`docker create` (run 44615350533) and `docker info` (run 44609059355). The layers above are
fixed numbers below what they wrap: `setup_capture` 20 s, install validation 60 s, harness
scripts 90 s, the test-support command timeout 10 s (`ffprobe` expired, run 44653479966) and
the PCM test waits (readiness 30 s over five launcher commands).

## Scope

`runtime.py` owns the budgets; every outer layer derives from them.

- `SETUP_BUDGET = 30` s: one deadline shared by engine selection (context/machine inspect,
  `getconf`, `info`, image inspect) and `create`. Six times the exceeded 5 s is a judgment;
  the logs show only that 5 s was exceeded.
- `CLEANUP_BUDGET = 15` s: shared by `rm`/`ps`/`inspect` and reaping the attachment.
- `OPERATION_TIMEOUT` (60 s) = both budgets + `LAUNCH_MARGIN` (15 s: interpreter, `start -a`,
  a short tool). `HARNESS_COMMAND_TIMEOUT` (90 s) adds 30 s for a short media command.
- `check_config_timeout(config)`: one operation per root plus the margin; install.py's
  `validation_timeout(config)` adds one operation per installed tool and the margin.

A `Budget` (name, seconds, expiry) reaches `capture`, `Engine`, `remove` and `reconcile`.
Timeouts read `engine command '<engine> <subcommand>' exceeded the <name> budget of <n> s`;
command failures name the command without the `--host` endpoint; a failed absence check keeps
the `rm` failure first. Consumers:
- install.py and `setup_capture` use the derived values; `issue-786-copied-audio-regression.py`
  and `real_owner_audio.py` import `runtime`/`install`: `validation_timeout` for the install
  call, `HARNESS_COMMAND_TIMEOUT` for other commands and the encoder-hiding wrapper.
- `voom-test-support` exports `LAUNCHER_COMMAND_TIMEOUT` (90 s) for `command_output` and
  `flac_command`, and that value times the commands enclosed: five for worker preflight, eight for
  one audio operation (input probe, encode, output probe, compatibility probe, copied-audio
  probes and packet scans). Expiry messages name the command or wait and its bound.
- `runtime_integration_test.py` startup waits allow setup plus margin; reaping waits allow
  cleanup plus margin. README states the budgets. No retries. Rejected: a longer per-command
  deadline, whose worst case grows with command count.

### Failure model

- Actors and deployments: a local operator on Linux/macOS Docker or Podman; the GitHub Linux
  amd64 PCM container job.
- Invariants: each layer exceeds the budgets it encloses; cleanup removes only the owned
  container; the Rust 90 s equals `HARNESS_COMMAND_TIMEOUT`.
- Accepted failure classes: setup over 30 s fails with the setup message; an unbounded tool run
  or witness hashing can exhaust a margin; removal after cancellation during create can take
  45 s; command counts drift if worker operations add launcher commands.
- Covered elsewhere: production worker startup deadlines (reported to the orchestrator).

## Success
1. Constants are pinned and each outer layer exceeds what it wraps.
2. Timeouts name the command or wait and its budget.
3. The `PCM container (Linux amd64)` job passes on the PR head.

## Validation

- Focused-test: `runtime_test.py` pins values, messages, budget sharing and the Rust constant.
- Focused-test: `install_test.py` pins `validate_install` and `validation_timeout`.
- Task-test-not-applicable (harness and PCM tests): they run only in the PCM CI job.
