# Media envelope adapter and commit-test reliability (#725)

## Problem and authority

The approved #339 canary supplies `rendered_payload.media_dispatch`; the node agent
reads `media_dispatch` at the top level. #725 and operator approval authorize the
adapter repair. The operator subsequently instructed “expand 725” to diagnose and
repair the failing commit-test helper and its direct regression once proven.
The pre-commit failure reports SQLite code 5 at `mark_pending_committed`'s first
insert after a read in a deferred transaction. Its actual competing writer remains
unobserved; isolated repetitions passing do not resolve the failure.

## Design

Read the existing workflow wrapper in the media adapter and retain the unchanged
strict envelope decoder, root resolution, fact checks and child/result handling.
Serialize the real workflow producer in unit fixtures; retain the wrapper through
API acquisition in lifecycle fixtures. Reject missing, malformed and flattened
payloads before a child, without a compatibility fallback.

For the test-only helper, first force contention with an independent writer and
zero busy timeout on each pooled connection. Invoke the actual helper in a task:
the old helper must fail at `file_versions insert`. Apply accepted ADRs 0083/0086
using `voom_store::tx::begin_read_then_write` with a distinct opener context.
The regression must then observe refusal at that opener, release the writer, and
prove the same helper successfully commits. This removes timing from the proof;
it neither retries nor increases a timeout. Existing ADR 0075 governs media decoding.
No ownership transition or new architectural decision is required.

## Success and verification

Media fixtures expose the original failure, then pass through strict decoding and
existing safety checks. The direct helper regression distinguishes opener failure
from a later write failure under a held lock and succeeds without contention.
Run focused media/lifecycle/commit tests, lint, source checks, mandatory hooks and
one final `just ci`. Run the unchanged eight-input retained canary at maximum four
files in flight; retain its evidence even if another defect stops execution.

## Failure model

Actors/deployment: authenticated owner agent with configured roots; local SQLite
unit fixtures and attended Linux canary. Protect strict decoding, root containment,
source bytes, existing authorization and durable lease/result semantics.
Accepted: test contention refuses immediately under deliberately zero busy timeout;
this test proves lock acquisition ordering rather than measuring wait latency.
Covered elsewhere: broader output/resume/capacity/full-library acceptance is #339.
No source-media mutation, production run, cleanup, DSL/schema/dependency/public API,
authorization, global timeout, staging/promotion or unrelated engine changes.
The operator retains production selection and retention decisions.
