# Container engine budgets (#897)

## Problem

`containers/ffmpeg/runtime.py` gives every engine command a 5 s `DEADLINE`. On a loaded
GitHub runner `docker create` (run 44615350533) and `docker info` (run 44609059355) exceeded it.
Enclosing layers are inconsistent too: one setup-tool child makes at least four serial
setup commands and a cleanup command (25 s) under `setup_capture`'s 20 s, and `install.py`
gives `check-config`, which runs one setup tool per root, a fixed 60 s.

## Scope

`runtime.py` replaces `DEADLINE` with two budgets, following the existing total cleanup budget:

- `SETUP_BUDGET = 30` s: one deadline shared by engine selection (context/machine inspect,
  `getconf`, `info`, image inspect) and `create`. Thirty seconds (six times the exceeded
  deadline) is a judgment: the logs show only that 5 s was exceeded.
- `CLEANUP_BUDGET = 15` s: one deadline shared by `rm`/`ps`/`inspect` reconciliation and
  reaping the attachment (was 5 s).
- `OPERATION_TIMEOUT = SETUP_BUDGET + CLEANUP_BUDGET + LAUNCH_MARGIN` (15 s for interpreter
  start, `start -a` and a short tool) bounds one launcher operation from outside, replacing
  `setup_capture`'s 20 s; `check_config_timeout(config)` allows one per root plus the margin.

A small `Budget` (name, seconds, monotonic expiry) is passed to `capture`, `json_capture`,
`Engine`, `remove` and `reconcile`. A timeout raises `engine command '<engine> <subcommand>'
exceeded the <name> budget of <n> s`. The nonzero-exit message names the command the same way
instead of printing `argv[1:3]` (`--host` and its endpoint). `install.py` passes
`check_config_timeout` to `check-config` and `OPERATION_TIMEOUT` to each `-version` check.
`runtime_integration_test.py` adopts the signatures; each wait enclosing a launcher step
(absence, check-config, `-version`, finish, barriers, sentinel cleanup) derives from the
constants. The README states the budgets. Media operations stay unbounded (the worker's lease
owns them). The 90 s wrappers in `scripts/issue-786-copied-audio-regression.py` and
`crates/voom-cli/tests/support/real_owner_audio.py` go to a follow-up issue. No retries.
Rejected: a longer per-command deadline, whose worst case grows with the command count.

### Failure model

- Actors and deployments: a local operator on Linux/macOS Docker or Podman; GitHub's Linux
  amd64 runner in the PCM container job.
- Invariants: each enclosing timeout exceeds the budgets it wraps plus `LAUNCH_MARGIN`;
  cleanup removes only the exact owned container.
- Accepted failure classes: setup slower than 30 s fails with the setup message; unbounded
  `start -a`, tool run or witness hashing can exhaust the margin; removal after cancellation
  during create can take 45 s.
- Covered elsewhere: the script wrappers (follow-up issue).

## Success

1. The constants are pinned, and each outer timeout exceeds the budgets it wraps.
2. A timeout in `create` or `info` names the command and budget.
3. Engine selection and `create` draw from one setup budget; cleanup gets a fresh budget.
4. The `PCM container (Linux amd64)` job passes on the PR head.

## Validation

- Focused-test: `runtime_test.py` pins the constants and the timeout relationship, checks the
  timeout and exit messages, and checks `setup_capture`'s timeout and that the guardian shares
  one setup budget.
- Focused-test: `install_test.py` checks the per-call timeouts `validate_install` passes.
- Task-test-not-applicable (runner load): the PCM container CI job on the PR head is the proof.
