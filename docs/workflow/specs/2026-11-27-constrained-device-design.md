# Constrained disk device discovery — #877

## Problem

The scheduled disk preflight in run 43911992178 exits during device discovery.
The wrapper always selects `lsblk PKNAME`, which is empty for whole disks.
The historic run does not log its mount source, so that topology remains an inference.

## Scope

The frozen #877 charter (q877-0ab62003) authorizes bounded discovery and tests.
Keep discovery in `scripts/run-constrained.sh`; no ownership transition is needed.
Resolve the mount source as today, stripping findmnt's bracketed subvolume suffix.
Query only that device's parent, without traversing its descendants. Use the parent
when present, otherwise retain the source, then require the existing block check.
Keep explicit `--device` and the exact write-bandwidth property unchanged.
Log the discovered source and selected device to make another failure diagnosable.
Use portable command doubles to exercise discovery and capture systemd arguments.
No ADR is needed: this restores the existing whole-device intent without a new contract.
New ENOSPC scenarios belong to #583; new stress coverage belongs to #577.

### Failure model

- Prevent: rejecting a valid whole disk because it has no parent; selecting descendant rows.
- Detect: unresolved or non-block sources fail before starting the constrained command.
- Recover: an operator may select the existing explicit `--device` override.
- Accept: unsupported layered storage still needs explicit device selection; historical
  topology cannot be established from the original log alone.

## Success

Whole disks and partitions select their whole device, including subvolume sources.
Explicit overrides skip discovery; invalid sources fail closed without running the command.
The wrapper passes the configured 40M write cap and memory/CPU controls to systemd/taskset.

## Validation

- focused-test: `just run-constrained-selftest` — portable fixture cases for device discovery,
  unresolved/non-block rejection, explicit overrides, and exact constraint arguments.
- focused-test: `just constrained-recipes-selftest` — callers retain constrained invocation.
- task-test-not-applicable: design prose has no runtime behavior; review and line/word caps.
- Integration: `just ci`, normal pre-commit hooks, and PR CI.
- On a capable host, inspect the selected device’s effective `io.max` inside the scope;
  report unavailable capability explicitly and distinguish it from argument capture.
