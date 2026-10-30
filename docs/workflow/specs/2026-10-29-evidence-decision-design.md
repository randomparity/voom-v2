# Phase-specific evidence decision (#791)

## Problem

Epic #787 Requirement 6 needs a reviewed phase table choosing identity evidence per phase,
and Requirement 7 needs rollout gates frozen at that decision. The measurements are merged
(#788, #789, #796, #797, #798, #801). No production trust may change before maintainer
approval, and a no-go is a valid outcome. #792–#794 are placeholders to be re-scoped here.

## Scope

Documentation only. No crate source, script, justfile or corpus measurement.

- [ADR 0120](../../adr/0120-phase-specific-media-evidence.md), status Proposed: the per-phase
  decision, its amendment of ADR 0077's agreement predicate for scan probes, and rejected
  alternatives with tagged grounds.
- `docs/media-evidence-decision-report.md`, the comparison report, with these sections:
  1. **Evidence**: one row per candidate (current, buf64k/buf1m/buf4m/batch8k, concurrency
     1/2/4/8, single-read, `meta`, `meta-ctime`, `ht*`, `rnd64-*`, lazy, R1, R2, R3,
     per-file readiness, storage version) giving source issue, record path, measured cost
     and protocol verdict, constructed misses, and selected or rejected. Numbers are copied
     from the committed records and READMEs, never recomputed or invented; unmeasured values
     say `not measured`.
  2. **Phase table**: for each of the six protocol phases, the seven Requirement 6 columns:
     required evidence, permitted actions before full verification, revalidation triggers,
     mismatch handling, supported deployments, residual risk, performance tradeoff.
  3. **Rollout gates**: numbered, frozen, each with metric, comparison, threshold (protocol
     thresholds unless a number is stated), corpus and cache state, and owner #794.
  4. **Implementation contracts**: bounded contracts for the selected changes (hash buffer;
     guarded probe reuse), including protocol, durable payload, setting, and compatibility,
     upgrade, downgrade, crash and rollback behaviour.
  5. **Re-scope drafts** for #792, #793 and #794: keep, rewrite, split or close, with draft
     text and dependencies, plus the variant if the trust change is rejected. Drafts only.
  6. **Not measured and exclusions**, carried from the records.
- `docs/adr/README.md`: one row for 0120 matching its neighbours (the index has no Status
  column).

The decision itself is fixed in ADR 0120; the report must not contradict it.

## Failure model

1. **Actors and deployments**: maintainer approving or rejecting ADR 0120; implementers of
   #792–#794 reading the contracts; the campaign orchestrator applying the re-scope drafts.
   Deployments: owner nodes with local btrfs or tmpfs roots, and network-mounted roots.
2. **Invariants and assets**: no committed claim contradicts a committed record; no shortcut
   is selected on speed alone; fingerprints stay distinct from content hashes; ADR 0050,
   ticket/lease authority and absence reconciliation unchanged; no host paths, mount or
   server names in committed text.
3. **Accepted failure classes**: modelled first-durable-observation gains may not
   materialise (accepted: the gates measure them and a miss blocks rollout); measurements
   from one host may not generalise (accepted: gates re-measure on the deployment).
4. **Covered elsewhere**: implementation #792/#793; rollout #794; SLO/CI #471; storage
   relocation #413/#528; issue edits by the orchestrator.

## Success

1. Every candidate in the evidence section has a selected or rejected verdict per phase, each
   tied to a record or README citation.
2. The phase table fills all seven columns for all six phases.
3. Every rollout gate has a numeric or protocol threshold and a named corpus.
4. The contracts name every protocol, payload and setting change in ADR 0120's Consequences.
5. Re-scope drafts cover #792, #793 and #794 in both approval outcomes.
6. `just ci` passes, including `check-adr-index`.

## Validation

- ADR index row — focused-test: `just check-adr-index` fails without the 0120 row, passes
  with it.
- Report and ADR content — task-test-not-applicable: human-readable decision text with no
  executable consumer; reviewed by the design and branch reviews against the cited records.
- Public safety — task-test-not-applicable: no structural checker exists; the author greps
  the diff for host paths, user names and mount or server names before push.
