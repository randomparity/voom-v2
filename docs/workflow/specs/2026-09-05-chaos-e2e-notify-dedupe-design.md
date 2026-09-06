# chaos-e2e notify dedupe — design

Issue: [#496](https://github.com/randomparity/voom-v2/issues/496)

## Goal

The `notify-failure` job in `.github/workflows/chaos-e2e.yml` reuses one open tracking
issue per distinct failure, instead of opening a fresh issue on every failed scheduled
run.

## Current behaviour

`notify-failure` runs `gh issue create` unconditionally with the fixed title
`Scheduled chaos-e2e run failed` and no labels. **Two distinct failures** have therefore
produced three identically-titled issues — #470, #491 and #536 — and the count grows by one
per failed run until the underlying failures are fixed. #496 names only the first two; #536
arrived after it was filed, which is the growth the issue predicts. None carries a label,
so none appears in a triage query filtering on `status:` or `type:`. That two different
defects collapsed into one title is the case for keying on the failing step, not an
incidental detail — see *The test half of requirement 4*.

## Requirements

1. Search for an existing open tracking issue before creating; do not create a duplicate
   when one exists.
2. When one is found, comment on it naming the new run, and stop.
3. When none is found, create it with `bug` and `status:needs-triage` at birth.
4. The title carries the failing step **or test** where it is cheap to extract, so two
   genuinely different `chaos-e2e` failures do not collapse into one issue. This design
   carries the **step** and not the test, deliberately — see *The test half of requirement
   4*, which states what that costs.
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
`if: failure()` step pipes `toJSON(steps)` through `jq`, and its stdout becomes the job's
`failing-step` output.

The filter takes the id of the first step whose `outcome` is `failure`, or `unknown-step`
when the context names none. Keying on the id
rather than the display name is what keeps the enumeration out of the workflow: the
context is already keyed by id, so a step added later with an `id:` is covered with no
second list to keep in sync, and a short slug reads well in a title. Only one step in a
job can fail — the job stops there — so "first" is a total order in practice, not a
tie-break.

The derivation is an inline `jq` filter in that step. An earlier draft extracted it to
`scripts/name-failing-step.sh` with a selftest wired into `just ci`, following
`scripts/select-ffmpeg-asset.sh` — the existing extraction from this same workflow file —
because the derivation is the part with a silent failure mode: a wrong filter yields
`unknown-step` for every failure and collapses every issue back into one, which is the
defect being fixed, invisibly. The operator declined that surface, confining this change to
`.github/workflows/chaos-e2e.yml` and explicitly excluding `justfile`. The filter therefore
ships untested by `just ci`; *Testing* records what was checked instead and what is left
uncovered.

## The test half of requirement 4

Requirement 4 says "the failing step **or test** where it is cheap to extract". This design
carries the step id and not the test name. The cost judgement, stated rather than left
implicit:

Extracting a test name means teeing the `Run Chaos Librarian E2E` step's stdout and parsing
`cargo test`'s `failures:` block. That is output-parsing machinery outside this change's
permitted surface, and it breaks whenever that output format moves — a coupling to a
libtest rendering detail, inside a notification path, to sharpen a key.

**What the step key buys, and what it does not.** Against the real history it would have
split the three duplicates two ways (`gh run view <id> --json jobs`, checked 2026-09-05):
#470 (run 31361822593) and #491 (run 32000334810) failed in `Run Chaos Librarian E2E`,
while #536 (run 32696142015) failed in `Install ffmpeg` with the E2E step never reached. So
the step key does discriminate on the evidence available, which is the case for adopting it.

What it does not buy is discrimination *inside* the modal bucket. `just chaos-e2e-ci`
(`justfile:249-252`) bundles three commands — `uv sync --locked`, a four-crate
`cargo build`, and `cargo test -p voom-cli --test chaos_librarian_e2e` — into the single
step `run-chaos-e2e` (`chaos-e2e.yml:109-110`). Within it, a lockfile-resolution failure, a
compile error, and two unrelated failing E2E tests all produce the title
`Scheduled chaos-e2e run failed: run-chaos-e2e` and land on one issue, which triage
separates by reading the comments. That bucket is where the recurring defect lives, so the
residual is real; it is accepted deliberately rather than overlooked.

Splitting `chaos-e2e-ci` into composed recipes so the step id discriminates would fix this
without any output parsing. It was put to the operator alongside the parsing option, and
both were declined in favour of shipping the dedupe fix now.

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

**Retitling the tracking issue detaches it**, and that is the second accepted consequence of
the same shape. The title is the whole key, matched exactly on both sides, so if triage
edits the issue's title — prefixing a component, appending a root-cause number, rewording it
to describe the actual defect — the next failure's comparison finds nothing and a fresh
issue is opened. It is silent: the log line is the ordinary "creating one", and the new
issue arrives labelled as though nothing were tracked. Closing it properly would mean a
second key — an invisible body marker or a dedicated tracking label — which is machinery a
notification path does not earn for a weekly nuisance that degrades only to today's
behaviour. The operational mitigation is a sentence, not code: on a tracking issue like
this, relabel and comment rather than retitle, or close it and let the next run open a
clean one.

The search is assigned from its own command rather than read through a pipeline, so a
failed `gh issue list` is attributable to `gh` instead of arriving at `jq` as empty stdin.
That is the same reason the `Install ffmpeg` step above writes its release read to a file,
and it is what keeps a transient search failure from being indistinguishable from "no
existing issue".

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
the naming step's `jq` filter on stdin; (b) the derived step name reaching an issue title, a
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
  the naming step in the producing job, and again on the value read from
  `needs.chaos-e2e.outputs.failing-step` in the consuming job. A check that ran only in the
  producing job would not be a control on what the privileged job consumes. Both sides
  carry `LC_ALL=C`, because the guard is a bracket range and glibc resolves a range through
  the locale's collation — under a UTF-8 locale it admits Arabic-Indic and fullwidth
  digits, so the export is part of the control rather than decoration. The exact-title
  comparison uses `jq --arg`, which is data, not program text.
- (b) The notify step runs under `set -euo pipefail`, and assigns the search result inside
  an `if !` guard rather than reading it through a pipeline. GitHub's default shell for a
  `run:` block with no `shell:` key is `bash -e`, *without* `pipefail`, so a failing
  `gh issue list` at the head of a pipeline would otherwise be masked by `jq` exiting 0 on
  empty stdin. What the pair establishes is precisely that a **failed** search fails the
  step, so a transient `gh` error cannot masquerade as "nothing found" and file a
  duplicate. It does not establish that an empty result set is trustworthy — a genuinely
  empty answer caused by search-index lag still costs one duplicate, which *Explicitly out
  of scope* accepts.
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

**Nothing in `just ci` exercises this change.** The repository has no actionlint and no
yamllint, workflow shell logic is not reachable from any recipe, and the extraction that
would have been testable is outside the permitted surface. That is the honest state, and
it is why the checks below were run by hand and recorded here rather than left implicit.

Performed on 2026-09-05, all read-only:

- **Both `run:` blocks extracted from the YAML and executed against fixtures.** The naming
  block returns `run-chaos-e2e` from a full success/failure context, `install-ffmpeg` when
  a setup step fails and later steps are `skipped`, and `unknown-step` when the context
  holds only successes, only `cancelled`/`skipped`, or a key outside the slug charset —
  the last with its `::warning::`. A non-object entry does not abort the filter.
- **The notify block against a stubbed `gh` recording its argv.** A failed search exits 1
  with `::error::` and issues no `create`; an exact bot-authored match produces a
  `gh issue comment <n>` and no `create`; a title matching only by containment, and an
  exact title from a non-bot author, both fall through to `create`; the `create` carries
  `--label bug` and `--label status:needs-triage`; and an empty or malformed
  `FAILING_STEP` warns and keys on `unknown-step`.
- **The search pipeline against the live repository.** With the old suffix-free key it
  selects #536 under `--state all` and nothing under `--state open`; with the new suffixed
  key it selects nothing under either. That is the containment/exactness contract, checked
  against real data.

Four things stay unchecked until a real scheduled failure, and are recorded here rather
than discovered later:

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
- **The naming step is inside the failing job**, so it yields no output whenever it does
  not run. Two of those are catastrophic and rare — the 60-minute timeout and a runner
  death, both of which Actions marks as `failure`. A cancellation is *not* one of them:
  `failure()` is false when an ancestor was cancelled, so `notify-failure` is skipped and
  nothing is filed at all. The third case is mundane and works differently: a **post** step
  failing. Post steps run after every main step, so if `Post Install uv` or
  `Post Cache cargo` fails while every main step succeeded, `if: failure()` is false when
  the naming step is evaluated, the step is *skipped*, and the job is then marked failed —
  so `notify-failure` fires with an empty output. All four degrade to `unknown-step` with
  the `::warning::` above rather than to a wrong key, but the fourth is why
  `unknown-step` issues should be expected occasionally rather than treated as alarming.
- **The search succeeds under a token scoped to `issues: write` alone.** This change adds
  a *read* call — `gh issue list --search`, which gh serves from the GraphQL search API —
  to a job that until now only wrote. Criterion 5 forbids widening the grant to find out,
  and the live check above ran under a developer credential rather than the job token, so
  it does not settle this. It could not be settled without merging. The failure here is
  **not** graceful, so the job is built to be loud about it: the search is assigned from
  its own command rather than through a pipeline, and a non-zero `gh` exit produces
  `::error::could not search for an existing tracking issue; refusing to file a possible
  duplicate` and fails the step. The job would then flip from filing a duplicate every run
  to filing nothing, visible only on the Actions run list. That trade is deliberate — a
  silent duplicate is worse than a named absence — but the first scheduled failure after
  merge must be checked for it specifically.

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
- **Put the failing test name in the key**, by teeing the E2E step's stdout and parsing
  `cargo test`'s `failures:` block. judgment: output-parsing machinery coupled to a libtest
  rendering detail, inside a notification path — and outside the permitted surface. Put to
  the operator with its residual stated and declined; see *The test half of requirement 4*.
- **Split `chaos-e2e-ci` into composed recipes** so the step id discriminates inside the
  E2E run. judgment: the cheaper way to sharpen the key, but it changes `justfile`, which
  this change's surface excludes. Put to the operator alongside the option above and
  likewise declined.
- **Extract the naming filter to `scripts/` with a selftest wired into `just ci`**, in the
  pattern of `scripts/select-ffmpeg-asset.sh`. judgment: the repository's own answer to
  untestable workflow shell, and it needs no permission change because the producing job
  already checks out — but it adds files the operator excluded from this change's surface.
  The consequence is recorded in *Testing*: the filter ships with no coverage in `just ci`.
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
