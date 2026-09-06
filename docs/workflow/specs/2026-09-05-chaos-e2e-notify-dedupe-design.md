# chaos-e2e notify dedupe — design

Issue: [#496](https://github.com/randomparity/voom-v2/issues/496)

## Goal

The `notify-failure` job in `.github/workflows/chaos-e2e.yml` reuses one open tracking
issue per distinct failure, instead of opening a fresh issue on every failed scheduled
run.

## Current behaviour

`notify-failure` runs `gh issue create` unconditionally with the fixed title
`Scheduled chaos-e2e run failed` and no labels. One defect has therefore produced three
identical issues — #470, #491 and #536 — and the count grows by one per week until the
underlying failure is fixed. #496 names only the first two; #536 arrived after it was
filed, which is the growth the issue predicts. None carries a label, so none appears in a
triage query filtering on `status:` or `type:`.

## Requirements

1. Search for an existing open tracking issue before creating; do not create a duplicate
   when one exists.
2. When one is found, comment on it naming the new run, and stop.
3. When none is found, create it with `bug` and `status:needs-triage` at birth.
4. The title carries the failing step, so two genuinely different `chaos-e2e` failures do
   not collapse into one issue.
5. No new token scope. `permissions: issues: write` on that job is not widened.

## Design

### The dedupe key is the issue title

`Scheduled chaos-e2e run failed: <failing-step-id>` — for example
`Scheduled chaos-e2e run failed: run-chaos-e2e`. One title, one tracking issue, one
failure streak. Requirement 1 names the title as the key, so the design does not choose
it; what the design chooses is the suffix that makes the key discriminate.

### Naming the failing step

Steps in the `chaos-e2e` job are not visible to the downstream `notify-failure` job, so
the name crosses as a job output. Every step in `chaos-e2e` gains an `id:`, and a final
`if: failure()` step pipes `toJSON(steps)` into a new `scripts/name-failing-step.sh`,
whose stdout becomes the job's `failing-step` output.

The script reads the steps context on stdin and writes the id of the first step whose
`outcome` is `failure`, or `unknown-step` when the context names none. Keying on the id
rather than the display name is what keeps the enumeration out of the workflow: the
context is already keyed by id, so a step added later with an `id:` is covered with no
second list to keep in sync, and a short slug reads well in a title. Only one step in a
job can fail — the job stops there — so "first" is a total order in practice, not a
tie-break.

The script exists rather than an inline `jq` call because the derivation is the part with
a silent failure mode: a wrong filter yields `unknown-step` for every failure and
collapses every issue back into one, which is the defect being fixed, invisibly. Off the
runner it is a pure stdin-to-stdout function, so it is directly testable — the same
reason `scripts/select-ffmpeg-asset.sh` was extracted from this same workflow file, and
this change follows that file's script + selftest + `just ci` recipe pattern exactly. It
runs in the `chaos-e2e` job, which already checks the repository out, so no job gains a
permission it did not have.

### The notify job

`notify-failure` reads `needs.chaos-e2e.outputs.failing-step`, falling back to
`unknown-step` when the output is absent — a job killed before its final step sets no
output, and a notification with a vague key beats no notification. Then:

1. Search: `gh issue list --state open --search "in:title … author:app/github-actions"
   --json number,title,author`, then select an exact title match whose author carries
   `is_bot`, using `jq --arg`. The search matches by phrase containment, so it is only a
   candidate filter; the exact comparison is the rule. The `author:` qualifier is
   server-side and load-bearing — see boundary (c) in the threat model.
2. Found: `gh issue comment <n>` naming the run, and stop.
3. Not found: `gh issue create` with the title, `--label bug`, and
   `--label status:needs-triage`.

Both labels exist in the repository, verified with `gh label list` on 2026-09-05. If one
is later renamed or removed, the create step fails and *no* notification is filed at all —
neither a labelled issue nor an unlabelled one. That is an accepted consequence, not a
safety property: the only channel it is visible on is the Actions run list, which is the
channel whose going unwatched is the reason this job exists. It is accepted because the
trigger is a deliberate label change rather than anything routine, and because the
alternative — retrying without `--label` — would violate requirement 3 in exactly the case
the labels are supposed to cover.

The job body stays inline. Running the extracted script here would need
`actions/checkout`, and that job declares its own `permissions:`, which replaces the
workflow default — so it would have to gain `contents: read`. Requirement 5 forbids
widening it, and a notification job is the wrong place to spend a permission.

### Migration

None required. #470, #491 and #536 all carry the old suffix-free title and are all closed
(verified 2026-09-05: the same `in:title` search returns them under `--state closed` and
returns `[]` under `--state open`). The job searches `--state open`, so the first scheduled
failure after this merges finds nothing, opens one issue under the new key, and reuses it
thereafter.

## Threat model

Security-relevant: the change edits CI config that handles `GH_TOKEN`, and builds an
issue title and a search query from a non-literal value.

**Boundary inventory.** Added: (a) the `toJSON(steps)` context reaching
`name-failing-step.sh` on stdin; (b) the derived step name reaching an issue title, a
search query, and an issue body; (c) issue titles and authors returned by the search
reaching the choice of which issue to comment on; (d) the job-output crossing
`needs.chaos-e2e.outputs.failing-step`, from the `chaos-e2e` job — which builds and runs
the whole workspace plus the `third_party/chaos-librarian` submodule — into the
`notify-failure` job, which is the one holding `issues: write`. Widened: none. The
`GH_TOKEN` handling and the `issues: write` grant are untouched.

Boundary (d) is the reason the charset check is applied twice. Anything able to write
`$GITHUB_OUTPUT` in the producing job would otherwise choose the string the privileged job
puts into a title, a query, and a body.

**Actor model.** The Actions runtime is trusted to report its own step outcomes, and the
step ids it reports are literals in the workflow file — a closed set. Any GitHub user may
open an issue on this public repository, which makes boundary (c) the only one an
untrusted actor reaches. Repository maintainers are trusted.

**Control per boundary.**

- (a) `set -euo pipefail`, `export LC_ALL=C`, no `eval`. Malformed or non-object stdin
  exits non-zero rather than emitting a partial name.
- (b) and (d) What actually holds at the privileged consumer is argv separation: the name
  reaches `gh` as its own argv element, never interpolated into shell program text, so a
  hostile value cannot become a second argument or a command. The charset check against
  `^[A-Za-z0-9_-]{1,64}$`, degrading to `unknown-step` with a `::warning::`, is defence in
  depth on top of that — and it runs on *both* sides of boundary (d): in
  `name-failing-step.sh` in the producing job, and again on the value read from
  `needs.chaos-e2e.outputs.failing-step` in the consuming job. A check that ran only in the
  producing job would not be a control on what the privileged job consumes. Both sides
  carry `LC_ALL=C`, because the guard is a bracket range and glibc resolves a range through
  the locale's collation — under a UTF-8 locale it admits Arabic-Indic and fullwidth
  digits, so the export is part of the control rather than decoration. The exact-title
  comparison uses `jq --arg`, which is data, not program text.
- (b) The notify step runs under `set -euo pipefail`. GitHub's default shell for a `run:`
  block with no `shell:` key is `bash -e`, *without* `pipefail`, so a failing
  `gh issue list` at the head of a pipeline would otherwise be masked by `jq` exiting 0 on
  empty stdin — the search would silently return "nothing found" and the job would file a
  duplicate, which is exactly the defect this change removes. A failed search must fail
  the step instead: a missed notification is recoverable on the next run, a duplicate is
  the bug.
- (c) What bounds this boundary is the **server-side** `author:app/github-actions`
  qualifier on the search, not the client-side check. An untrusted actor has two moves
  against a predictable title, and the client-side check only stops the first. *Capture*:
  open an issue under the exact title so the job comments there instead of filing — the
  `author.is_bot` check in `jq` rejects that, and no human account can set the flag.
  *Eviction*: open 50 or more issues whose titles merely **contain** the key, so that
  phrase-containment search plus `--limit 50` pushes the real tracking issue out of the
  returned window; the exact-title filter then matches nothing and the job files a fresh
  duplicate every week, under the actor's control and invisible in the log. Filtering by
  author on the server keeps those issues out of the result set entirely. The client-side
  `is_bot` and exact-title checks stay as defence in depth.

**Explicitly out of scope.** A compromised `GITHUB_TOKEN` or a malicious maintainer: this
job's grant is unchanged, so the design neither adds nor removes that exposure. Search
index lag causing one duplicate issue: an availability nuisance bounded by the weekly
cadence, and no worse than today's behaviour, which duplicates every run.

## Testing

`scripts/name-failing-step-selftest.sh`, wired as `just name-failing-step-selftest` into
the `ci:` recipe and a prek hook, in the pattern of `select-ffmpeg-asset-selftest`. Each
case pins one rule: the failure is selected over successes and skips; a context with no
failure yields `unknown-step`; an unparseable or non-object context exits non-zero; a
name outside the slug charset degrades to `unknown-step`; empty stdin exits non-zero.

The `notify-failure` job body gets no *in-job* executable coverage: running the extracted
script inside that job would need `actions/checkout`, and therefore `contents: read`,
which requirement 5 forbids. What that constraint does *not* foreclose is checked anyway —
the plan runs the search-and-select pipeline against the live repository read-only from a
developer machine and pins its output, which is what proves the `jq` selector and the
search interaction behave as designed.

Three assumptions remain unverified until a real scheduled failure, and are recorded here
rather than discovered later:

- **Failed-job output propagation.** Criterion 4 rests on `needs.chaos-e2e.outputs.*`
  being readable by a dependent job when the producing job *failed*. This is the first
  `needs.<job>.outputs` reference in this repository, so no local precedent confirms it,
  and it cannot be exercised without an Actions run. The failure is graceful rather than
  silent-and-wrong: an empty output degrades to `unknown-step`, and the notify step emits
  a `::warning::` annotation saying the output was absent, which is what distinguishes
  this case from a genuine `unknown-step`.
- **`workflow_dispatch` cannot exercise the path.** The job's pre-existing gate is
  `if: failure() && github.event_name == 'schedule'`, so the first real execution is a
  scheduled failure. That gate is pre-existing and out of scope to change.
- **A `Checkout` failure necessarily degrades to `unknown-step`**, because
  `scripts/name-failing-step.sh` lives in the tree the failed step was fetching. The
  namer cannot name its own missing checkout.
- **The search succeeds under a token scoped to `issues: write` alone.** This change adds
  a *read* call — `gh issue list --search`, which gh serves from the GraphQL search API —
  to a job that until now only wrote. Criterion 5 forbids widening the grant to find out,
  and Step 2.5a runs under a developer credential rather than the job token, so it does
  not answer this. Unlike the three assumptions above, the failure here is **not**
  graceful: `set -euo pipefail` turns a rejected search into a failed step, so the job
  would flip from filing a duplicate every run to filing nothing at all, visible only on
  the Actions run list. That trade is the right one — a silent duplicate is worse than a
  loud absence — but the first scheduled failure after merge must be checked for it
  specifically.

No static check covers the workflow's Actions schema or its expressions: `just ci` carries
no actionlint and no yamllint. Adding one would be a repository-wide tooling change beyond
this issue's surface, and is reported as follow-up work rather than taken here.

## Out of scope

The underlying `chaos-e2e` failure (#470, #491; root cause #497). The identical defect in
`.github/workflows/constrained-resources.yml` and `.github/workflows/net-resilience.yml`,
which #496 scopes out and this change tracks as follow-ups.

## Decisions and rejected alternatives

No ADR. The one decision with genuinely open alternatives — what the dedupe key is — was
settled by the issue's own requirement, and a record whose decision was made elsewhere
records nothing. The alternatives below are kept here, where the change that needs them
lives.

- **Do the work inside the notify job** — either reading the failing step from
  `GET /actions/runs/{id}/jobs`, or checking the repository out so the extracted script can
  run there. verified: the first needs `actions: read` and the second needs
  `contents: read`, and that job declares `permissions: issues: write`
  (`.github/workflows/chaos-e2e.yml:119-120` at 16b62e87), which *replaces* the
  workflow-level default rather than adding to it — every scope not listed is `none`. Both
  therefore widen the grant requirement 5 freezes. This one fact rules out both, and it is
  why the derivation runs in the `chaos-e2e` job, which already checks out.
- **Enumerate step display names in an env block** rather than keying on `toJSON(steps)`.
  judgment: a second list of every step, which a later step addition silently falls out
  of, to gain a prettier suffix than the id.
- **Match on the search result alone, without the exact-title comparison.** verified:
  `in:title` matches by phrase *containment*, not equality —
  `gh issue list --repo randomparity/voom-v2 --state all --search 'in:title "Scheduled
  chaos-e2e run failed"'` returns #470, #491 and #536, and would equally return any title
  carrying that phrase plus a suffix (gh 2.97.0, 2026-09-05). Containment is
  one-directional, so without the exact comparison a shorter key swallows every longer
  one, recollapsing the failures requirement 4 exists to separate.
- **Identify the bot by `author.login == "github-actions[bot]"`.** verified: `gh` reports
  that account as `{"is_bot": true, "login": "app/github-actions"}` on #470, #491 and #536
  (`gh issue list --json author`, gh 2.97.0, 2026-09-05) — the `[bot]` form is the REST
  spelling, not gh's. Matching that literal would never match, so every run would create a
  duplicate: the defect being fixed, reintroduced silently. `is_bot` is the stable field,
  and no human account can set it.
