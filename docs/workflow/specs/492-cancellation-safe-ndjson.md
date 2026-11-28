# Cancellation-safe NDJSON reads (#492)

## Problem and authority
Issue #492 reports malformed worker progress during a historical CI run. Current
`execution/dispatch.rs` selects `NdjsonReader::next_frame` against heartbeats.
`next_frame` clears its line buffer before awaiting additional bytes, so cancelling
and restarting that future loses an already-consumed prefix. This establishes a
current defect; it does not establish the historical run's cause.
Authority: WORK:SCOPE token q492-f2c3d7f8 on issue #492.

## Design
The protocol reader remains the sole owner of framing state. Retain `line_buf`
until a complete line has been decoded; clear it synchronously after decoding
and before lease/sequence/terminal processing. There is no suspension between
completed line acquisition, decode, validation, and outcome. Duplicate frames
must clear their complete line before continuing to the next line. No public
signature, wire shape, allocation bound, timeout, or validation ordering changes.
Document that cancellation preserves partial bytes when the same reader is reused;
errors still abort the stream. The workflow consumer requires no production edit.
Holding a pinned future in the dispatcher alone would leave other shared-reader
consumers exposed and complicate ownership; repair the existing state owner.

## Success
A deterministic partial-frame cancellation completes the original frame on reentry.
Repeated cancellations, duplicate delivery, coalesced frames, fragmented terminal
frames, wrong leases, sequence gaps, frame limits and mid-frame EOF preserve their
existing outcomes. A workflow test drives its real heartbeat-select consumer using
a benign in-memory reader that yields a prefix, Pending, then the remaining bytes;
the immediate interval tick cancels the first read before terminal completion.
No timing sleeps, relaxed assertions, or external worker process are required.

## Failure model
In scope: cancellation while waiting for a line's remaining bytes; duplicate
skipping followed by cancellation; stream fragmentation/coalescing; lease, sequence,
terminal, EOF and size checks across reentry. Tests use explicit poll boundaries.
Accepted: ProtocolError aborts the stream; recovery after such an error is unsupported.
Excluded: PID startup/reaping (owner-node cutover), worker binary selection (#463),
chaos end-to-end (#491/#470), throughput arithmetic (benchmark fixture). Future
watchdog scheduling limitations are independent of repaired byte retention.

## Validation and rollback
Focused regressions fail with malformed/truncated frames before the fix and pass
after it. Run protocol tests, workflow regression, repository layout/fmt/lint,
then `just ci`, mandatory commit hooks and hosted exact-head checks. Reverse this
single reader change and its tests to roll back; no migration or data repair exists.
