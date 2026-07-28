# First-extraction primary bundle creation

Issue: #385

## Goal

Allow a published audio extraction whose exact active source version was scanned
without sidecars to establish its primary bundle before dispatch. Bundle identity
creation and the first planned extraction ledger must commit together so failure
cannot leave a provisional work, variant, bundle, membership, or extraction row.

## Current state

Scan creates the provisional work, variant, bundle, and `primary_video`
membership only when it discovers sidecars. The assembly and its four events live
inside `scan::persist`.

The workflow adapter requires that membership before it constructs
`ExecuteExtractAudioInput`. The audio executor later creates the planned
`audio_extract_operations` row in a separate `BEGIN IMMEDIATE` transaction.
Moving only the scan assembly into a reusable helper would therefore still allow
bundle identity to commit when extraction planning rolls back.

Existing extraction from an established bundle also has a strict legacy-owner
adoption path. Lazy creation must not bypass or reinterpret that path.

## Decisions

### One shared primary-bundle assembler

Add a crate-private media-bundle use-case operation that runs in a caller-owned
transaction. It accepts an exact source file-version ID, a display name, and an
observation time.

The operation:

1. loads the exact file version inside the transaction;
2. requires it to be the greatest live version for its asset;
3. reuses an existing `primary_video` membership without writing rows or events;
4. rejects an existing non-primary membership as a conflict; or
5. creates one provisional unknown work, one provisional `scan` variant, one
   bundle, and one `primary_video` membership.

The create path emits exactly one event for each created identity, in this order:

1. `media_work.created`;
2. `media_variant.created`;
3. `asset_bundle.created`;
4. `asset_bundle.member_added`.

Scan calls this operation from its existing persistence transaction instead of
owning a second copy of the assembly. Scan still creates a bundle only when it
has sidecars; a primary-only scan remains unchanged until extraction needs the
bundle.

### Atomically plan first extraction

Before the normal workflow extraction path, inspect whether the source asset is
already a bundle member.

- When a membership exists, require `primary_video` and continue through the
  existing executor. This preserves legacy singleton adoption before a new
  planned operation is inserted.
- When no membership exists, compute the same source snapshot selection,
  canonical target paths, operation key, and ordered output descriptors that
  normal execution will use. Then start `BEGIN IMMEDIATE`, invoke the shared
  primary-bundle assembler, and create or exact-replay the planned extraction
  operation and outputs in that transaction.

Expose the operation repository's validated planned insertion as an in-
transaction method. Its standalone `create_planned` method remains the same
transaction-owning wrapper around that method.

After the transaction commits, construct the existing
`ExecuteExtractAudioInput` with the resolved bundle ID. Normal execution
revalidates the source, recomputes the selection and paths, and exact-replays the
planned ledger before dispatch. This duplicate read-side validation is
intentional: the pre-dispatch executor remains authoritative and no unchecked
prepared object crosses the workflow boundary.

### Concurrency and rollback

`BEGIN IMMEDIATE` is the linearization boundary for concurrent first
extractions. One writer creates the bundle and planned operation. A later writer
observes that state and exact-replays it. The schema's
`UNIQUE(asset_bundle_members.file_asset_id)` remains the one-bundle-per-asset
fence; no new schema constraint is required.

Any failure while creating the work, variant, bundle, membership, their events,
the extraction operation, or an extraction output rolls back the complete
transaction. The workflow executor subsequently releases the acquired lease and
records exactly one `lease.released` plus one terminal ticket-failure event. The
ready-to-leased transition remains visible because the workflow already owns its
lease. Terminal failure leaves the ticket attempt at the value established by
lease acquisition and advances the ticket and lease epochs once while moving
them to `failed` and `released`. No bundle-creation or
audio-started/audio-failed event survives: execution input was never
constructed.

Canonical target-path preparation retains its existing behavior and may create
the configured empty target directory before the transaction. A planning
failure sends no worker request and creates no staging leaf or target file. The
rollback guarantee covers every durable work, variant, bundle, membership,
extraction, and event row created by first-extraction planning; it does not
claim rollback of the pre-existing target-directory preparation contract.

Once a planned operation commits, later worker, verification, or publication
failure keeps that operation and bundle as resume state. That is not partial
creation: retry must reuse the same identities and operation key.

## Compatibility

This change adds no migration and changes no public DSL, compiled-policy JSON,
worker protocol, CLI envelope, event payload, durable payload shape, or public
error code. Existing audio execution reports keep the resolved bundle ID.
Existing scans without sidecars still report no bundle.

## Test strategy

1. Bundle use-case tests prove exact active-version validation, create-event
   order, no-event reuse, and non-primary rejection.
2. A planning failure injected after bundle assembly proves zero new work,
   variant, bundle, membership, extraction-operation, extraction-output, and
   bundle/audio event rows, plus the exact workflow lease-release and terminal
   ticket-failure facts.
3. Concurrent first planners on separate pool connections return one bundle and
   one exact operation, with one four-event creation sequence.
4. Workflow/executor tests prove no dispatch while transactional planning fails,
   normal dispatch after success, and idempotent resume without duplicate rows
   or events.
5. The real audio extraction integration removes test-only bundle seeding and
   asserts the primary plus extracted members and exact creation events.
6. The shipped-CLI published grammar process test runs its audio scenario from
   primary-only scan state through real worker supervisors and succeeds without
   store seeding.

## Rejected alternatives

### Create the bundle in the workflow adapter, then plan normally

This leaves durable provisional identity when planned extraction insertion
fails and does not meet the rollback requirement.

### Make scan create a bundle for every primary

That changes scan's durable and envelope contract for all libraries and creates
identity before any consumer needs it.

### Add a bundle ID sentinel or optional durable payload field

This weakens type and event invariants and changes contracts unnecessarily. The
workflow resolves the real bundle ID before constructing the existing execution
input.

### Bypass legacy adoption for every established bundle

That can reject or duplicate previously committed singleton extraction state.
Only the membership-absent first-creation path inserts the plan atomically with
the new bundle.
