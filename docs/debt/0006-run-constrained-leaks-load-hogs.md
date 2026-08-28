# 0006 — `run-constrained.sh` leaks its `--load` busy-loops

## Status

Open
review-by: 2026-11-27

## Concern

`scripts/run-constrained.sh --load N` spawns `N` busy-loops per CPU to create
competing load, and never reaps them. The cleanup is registered as a shell trap:

- `trap cleanup EXIT INT TERM` at `scripts/run-constrained.sh:150`
- hogs spawned into `hogs+=($!)` at `:152-160`
- `exec systemd-run --user --scope … -- taskset -c "$CPUS" "$@"` at `:177`

`exec` replaces the shell process, so the `EXIT` trap never fires and every hog
is reparented to init and spins until killed by hand.

Reproduced on Fedora, Linux 7.1.8-200.fc44.x86_64, repo at `31248beb`:

```
$ ./scripts/run-constrained.sh --cpus 0 --load 1 -- true
run-constrained: 1 competing loops pinned to cpus 0
$ echo $?
0
$ pgrep -af 'sh -c while :'
265885 sh -c while :; do :; done
```

At the tool's default `--cpus 0-3` with `--load 1`, that is four orphaned
busy-loops per invocation.

`just run-constrained-selftest` does not catch it: `--print-plan` returns at
`:128`, before the hog block, so the selftest never exercises the `--load` path.

## Why deferred

`scripts/` is outside the frozen change surface for issue #592
(https://github.com/randomparity/voom-v2/issues/592#issuecomment-5447684231,
token `q592-387107cd`), whose surface is the crates on the deadlocked path plus
`docs/`.

The fix is also not a one-liner. Dropping `exec` would keep the shell alive to
run the trap, but it changes the process tree the scope wraps and the exit-status
path that `systemd-run --scope` currently propagates directly, both of which the
acceptance protocol in
`docs/workflow/plans/2026-08-27-cancelled-begin-immediate-lock-leak.md` depends
on. Whatever replaces it needs a selftest case that exercises `--load`, which
`--print-plan` currently short-circuits.

## Impact where it was found

Issue #592's acceptance sweep runs `run-constrained.sh --load 1 --write-bps 40M`
up to 180 times. Uncompensated, that accumulates roughly 360 spinning processes
pinned to four CPUs across the pre-fix arm alone, with the post-fix arm starting
on top of them. The runs then get monotonically heavier, which destroys the
comparability the per-run invocation exists to provide, and CPU starvation can
trip `HANG_GUARD` for reasons unrelated to the defect under test — and the
sweep's reproduction predicate *is* a `HANG_GUARD` message, so the two are
indistinguishable in the log.

That sweep compensates in its own loop (reap after each run, abort if any hog
survives into the next) rather than modifying the script.

The compensation is itself constrained by this defect. Because `exec` reparents
the hogs to init, they are not in the sweep loop's process tree, so the reap has
to match on the command-line pattern host-wide (`pkill -f '^sh -c while :'`). That
kills *any* concurrent `run-constrained.sh` invocation's load generators too,
silently. Until the script reaps its own, **no two `run-constrained.sh` users can
share a host safely** — which is a second reason to fix it at the source rather
than in every caller.

The anchor is load-bearing and was found the hard way. `pgrep -f`/`pkill -f` match
the entire command line of every process, so an unanchored
`'sh -c while :; do :; done'` also matches the shell running the reap, whose own
command line contains the pattern as text. Measured with one real hog alive: the
unanchored form counts **2**, the anchored form **1**. A caller compensating for
this defect with the unanchored pattern gets a leak guard that fires when nothing
leaked, and a reaper whose match set includes the process doing the reaping.

## What would resolve it

`run-constrained.sh` reaps its own hogs on every exit path, plus a
`run-constrained-selftest` case that spawns under `--load`, returns, and asserts
no `^sh -c while :` process survives. Done when that case fails against the
current script and passes against the fixed one.

## Provenance

target: scripts/run-constrained.sh
target: scripts/run-constrained-selftest.sh
Raised by `$gauntlet` during `$trial-loop` on the issue #592 implementation plan,
iteration 4, 2026-08-27, under `$quest` for issue #592 (scope token
`q592-387107cd`). Reproduced independently before filing.
tracker: #592
