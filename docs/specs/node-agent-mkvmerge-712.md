# Explicit MKVToolNix dependency for node agents (#712)

## Problem

The agent clears its child environment and has no MKVToolNix executable setting.
The bundled remux worker consequently rejects the bare `mkvmerge` fallback.
The operator approved this separate prerequisite for #339 after a differential
startup probe failed without, and succeeded with, `VOOM_MKVMERGE_BIN`.

## Scope

Extend the existing `WorkerDependencyPaths` with optional `mkvmerge_bin`.
Require it for workers declaring `remux` and reject it for other workers,
using the existing absolute/existing/regular/executable validation helper.
Pass its configured value as `VOOM_MKVMERGE_BIN` in the existing cleared child
command. Keep debug output redacted consistently with sibling dependency paths.
Update the node-agent runbook with a complete remux provisioning example.
This is an extension of existing ownership and dependency policy, with no new
architecture decision, public command, dependency, database field or media work.
Authority: #712 WORK:SCOPE q712-d8193165 and explicit operator approval.

## Failure model

One operator supplies strict local TOML and installs the matching worker/tool.
Missing, relative, nonexistent, non-file or non-executable remux dependencies
must fail configuration before activation. An irrelevant dependency must fail.
Executable replacement after validation remains the existing operator trust
boundary; no filesystem sandbox, arbitrary environment forwarding or new
executable-integrity guarantee is introduced. Tool preflight/version failures
remain the worker's responsibility. #339 remains blocked until this PR merges.

## Success and validation

Remux config fails without a usable dependency; other workers retain existing
requirements. Parsed configured values reach the child without ambient PATH,
FFmpeg or NVIDIA variables. Debug formatting omits the configured location.
Tests cover the configuration error table, unrelated-worker rejection and exact
child environment using existing fixtures; controlled omission must fail.
A real bundled MKVToolNix child must reach ready through the node agent with
the installed executable, without wrappers or PATH restoration. Retain process
and durable readiness evidence. Run focused config/child tests, format, lint,
required hooks and one final `just ci`; then independent review and CI handoff.
No canary/full production execution or evidence deletion belongs to this PR.
