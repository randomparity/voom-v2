# Issue #332: Filter-addressed remux implementation plan

- Issue: #332
- Branch: `feat/filter-addressed-remux`
- Base: `c11b3623dbf5b6702cdadbc91a45ae8a420ffbf6`
- Design:
  `docs/superpowers/specs/2026-07-26-issue-332-filter-addressed-remux-design.md`

## Success criteria

- Both published filter-addressed forms resolve against retained structured
  stream facts.
- Zero and multiple retained matches block the file with distinct actionable
  diagnostics.
- Planning and execution consume one resolved snapshot identity.
- Bare head-only ordering preserves remaining source order.
- Produced defaults and order are inspected, and a compliant output replans
  `NoOp`.
- Existing compiled policies and old remux payload/event shapes remain
  readable.
- #336 best selection remains separate and cannot override explicit resolved
  defaults.

## Step 1 — Pin payload compatibility and resolution failures

Add failing `voom-plan` tests for:

1. old remux payloads parse with no resolved IDs;
2. resolved default/head IDs round-trip;
3. empty or malformed resolved IDs fail parsing;
4. zero/multiple defaults matches produce their diagnostic codes/messages;
5. zero/multiple order-head matches produce their diagnostic codes/messages;
6. matching only a removed stream counts as zero retained matches.

Implement additive payload fields and one shared retained-stream cardinality
resolver.

Focused guardrail:

```bash
cargo test -p voom-plan --all-features filter_addressed
cargo clippy -p voom-plan --all-targets --all-features -- -D warnings
```

## Step 2 — Make planning and payload construction share one result

Refactor remux track evaluation to return resolved default actions, optional
head identity, group order, and final change status. Use it for both node status
and payload construction.

Add behavior tests for:

1. explicit default selection changes and `NoOp`;
2. head-only order changes and `NoOp`;
3. head-plus-group order changes and `NoOp`;
4. stable provider-stream order tie handling;
5. attachments excluded from head candidates.

Focused guardrail:

```bash
cargo test -p voom-plan --all-features remux
```

## Step 3 — Populate and validate execution selection

Add failing `voom-control-plane` tests for:

1. resolved default emits one set ref and clears every other kept target;
2. resolved head populates `head_streams`;
3. empty `track_order` is valid only with a resolved head;
4. removed, missing, wrong-kind, and duplicate resolved identities fail with
   actionable configuration errors;
5. strategy defaults remain unchanged.

Implement pinned-snapshot ID validation without filter reevaluation.

Focused guardrail:

```bash
cargo test -p voom-control-plane --lib remux::selection::tests
cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings
```

## Step 4 — Record head selection in durable events

Add default-empty `head_streams` to all durable remux event content structs and
populate them from the execution selection. Test old JSON deserialization and
started/progress/succeeded/failed event visibility.

Focused guardrail:

```bash
cargo test -p voom-events --all-features artifact_remux
cargo test -p voom-control-plane --lib remux
just check-payload-deny-unknown
```

## Step 5 — Prove generated-media behavior

Change the generated remux policy to use:

```text
defaults audio where language == "eng" and not commentary
order tracks [video, audio, subtitle] where language == "eng" and not commentary
```

Assert:

1. the selected main audio is the only default audio;
2. it is the first ordinary output track;
3. video, retained subtitle, and attachment follow deterministically;
4. commentary and attachment dispositions remain correct;
5. the produced snapshot replans compliant/`NoOp`.

Focused guardrail:

```bash
cargo test -p voom-control-plane --all-features --test remux_flow -- --nocapture
```

## Step 6 — Documentation, review, and ship

Update ADR 0023 and the control-plane design spec to remove the interim inert
state. Run focused checks, `just ci`, adversarial review, simplification review,
and the review loop. Rebase on current `main`, rerun `just ci`, publish one PR
closing #332, wait for all required checks, and merge serially before starting
#336.
