# 0004 — Transitional local scan paths are not race-free

## Status

Open
review-by: 2026-10-31

## Concern

The issue #418 local scan adapter canonicalizes every discovered primary and sidecar beneath the
selected node-owned root before launch and again before byte access. On Unix, the final file open
also uses `O_NOFOLLOW`. These checks reject stable symlink escapes and leaf-symlink replacement,
but they do not bind ancestor directories or the later path-bearing worker request to an open
filesystem object. A concurrent ancestor rename followed by an out-of-root symlink can therefore
race the transitional control-plane read or the worker's pathname reopen.

## Why deferred

Issue #418 owns the durable root and provider-relative location schema, exact-local validation,
and bounded rooted-result plumbing. ADR 0055 explicitly leaves worker protocol and worker
implementations unchanged here. Issue #421 owns moving discovery, hashing, and symlink-safe scans
to the storage-owner node agent; issue #423 owns replacing path-bearing worker requests with stable
owner-local references. Implementing descriptor-relative traversal or handle passing in #418 would
duplicate or preempt both contracts.

## Non-regression boundary

Issue #418 keeps the unchecked path scan entry private. Public scans require an effectively active
root owned by the explicitly configured local node, canonicalize the configured root, validate
every primary and sidecar after discovery and filesystem grouping, reject leaf symlinks with
`O_NOFOLLOW` on Unix, and derive the persisted provider-relative locator only beneath the selected
root. These checks are defense-in-depth; they must not be described as closing concurrent ancestor
replacement or as making path-bearing worker dispatch stable.

## What would resolve it

Complete #421 with owner-node discovery and hashing that binds every byte read to a root-anchored,
symlink-safe filesystem object, including deterministic ancestor-replacement regressions for
primary and sidecar inputs. Complete #423 with a worker dispatch reference that stays bound to that
validated object through probing and no longer reopens an absolute pathname. Both paths must fail
closed without reading, dispatching, or persisting an out-of-root object.

## Provenance

target: crates/voom-control-plane/src/scan/hash.rs
Challenge run `challenge-418-impl-r3-20260805` found the concern on 2026-08-04.
tracker: #421
Related tracker: #423
