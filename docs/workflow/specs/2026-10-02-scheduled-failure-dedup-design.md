# Reuse scheduled failure tracking issues (#620)

## Problem and scope

The constrained-resources and net-resilience notification jobs unconditionally
create issues. Port the established chaos-e2e pattern requested by #620.
Each existing job continues owning its notification; its fixed title is unchanged.
No ownership migration or new shared abstraction is needed. A shared executable
would require checkout in these currently checkout-free notification jobs.
No new architectural decision is made; optional ADR 0111 remains unused.

## Behavior and success

Search open issues using the existing fixed title and server-side
`author:app/github-actions`, with `--limit 50 --json number,title,author`.
Select the first exact-title result whose `.author.is_bot?` is true using jq.
Comment with the failed run URL when a match exists. Otherwise create one issue
with `bug` and `status:needs-triage` labels and the existing workflow description.
Use `set -euo pipefail`; capture the gh search separately and fail with
`::error::` when it fails, before any comment or creation. jq and write failures
also fail the step. Preserve job permissions, schedule-only conditions and titles.

## Failure model

Deployment: the two existing scheduled Ubuntu Actions notification jobs, with
GitHub job tokens and their current serialization. Assets: correct issue routing
and no creation caused by a failed search. Humans may create similarly titled
issues; server-side bot qualification and exact-title/bot selection exclude them.
Search-index lag and independently concurrent external writers can still create
duplicates; this port accepts that existing #619 residual, rather than promising
atomic deduplication. A failed comment/create is surfaced without retry because
write outcome can be ambiguous. Job-token search permission is unverified until
the first real scheduled failure; failure is loud, never interpreted as absence.

## Validation and exclusions

Execute each workflow's actual extracted run block with mock gh and real jq:
existing bot match comments; empty or nonmatching results create with both labels;
failed search and malformed JSON fail without writes; comment/create errors fail.
Include similar human titles and absent author metadata. Assert search arguments
and run URL propagation. Baseline scripts parse; initial behavioral controls must
fail because the old implementation creates without searching.
Native hooks and final required `just ci` remain gates. Do not inject a public
scheduled failure. General static workflow tooling belongs to #621;
notification-key redesign needs separate authorization; duplicate cleanup and
live failure injection remain separately authorized operator actions.
