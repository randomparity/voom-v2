# chaos-e2e notify dedupe — implementation plan

Goal: make the `notify-failure` job in `.github/workflows/chaos-e2e.yml` reuse one open
tracking issue per distinct failure instead of filing a new one every failed scheduled run.

Spec: `docs/workflow/specs/2026-09-05-chaos-e2e-notify-dedupe-design.md`.

Architecture: the `chaos-e2e` job gains a step `id:` on every step and a final
`if: failure()` step that pipes `toJSON(steps)` through `jq`, publishing the failing step's
id as the job output `failing-step`. The `notify-failure` job reads that output, builds the
key `Scheduled chaos-e2e run failed: <step>`, searches open issues for an exact
bot-authored title match, and either comments on the match or creates the issue with `bug`
and `status:needs-triage`.

Expected implementation size: 100–130 changed lines (S/M) — one file, from the file map below.

## Global Constraints

- Base branch `main`; branch `feat/dedupe-chaos-notify-496`.
- Guardrail command: `just ci`, run bare — no pipes, no `>/dev/null`, no `|| true`. CI
  invokes it directly, so every sub-check hard-gates the PR.
- **One file.** `.github/workflows/chaos-e2e.yml` and nothing else. The operator's
  criterion-4 authorization states that it does not expand file scope beyond that file and
  does not permit touching `justfile`. An earlier revision of this plan extracted the
  naming filter to `scripts/name-failing-step.sh` with a selftest wired into `just ci`;
  that is out of scope and was removed.
- **No `permissions:` block may change**, in any workflow. Completion criterion 5 forbids
  widening the `notify-failure` grant, which is and stays exactly `issues: write`.
- No output-parsing machinery — no teeing the E2E step's stdout, no parsing `cargo test`.
- `jq` is preinstalled on `ubuntu-latest`.
- Repository labels `bug` and `status:needs-triage` both exist and are used verbatim.
- Do not touch `.github/workflows/constrained-resources.yml` or
  `.github/workflows/net-resilience.yml`. They carry the identical defect and are
  explicitly excluded.
- No ADR, and therefore no `docs/adr/README.md` row.

## File map

| File | Status | Answerable for |
|---|---|---|
| `.github/workflows/chaos-e2e.yml` | modified | Step ids, the `failing-step` job output, the naming step, and the dedupe logic in `notify-failure`. |

## Verification inventory

One material contract changes, and it splits into two halves with different modes.

- **The naming filter and the search-and-select pipeline.**
  `Mode: focused-test`. Both are pure functions of their input — a steps-context JSON
  object, and a `gh issue list` JSON array — so both are executable off a runner. Task 3
  extracts each `run:` block from the YAML and drives it against fixtures and a stubbed
  `gh`, and runs the search pipeline against the live repository read-only. Expected
  results are enumerated there.
- **The two `gh` write calls.**
  `Mode: task-test-not-applicable`. Changed surface: `gh issue comment` and
  `gh issue create`. Reason: observing either means creating a real issue or comment on the
  live repository, which is a write this change must not perform to verify itself; and the
  job cannot be exercised in place, because its pre-existing gate is
  `if: failure() && github.event_name == 'schedule'`, so `workflow_dispatch` never reaches
  it. What *is* checkable about them — that the create carries both labels and the correct
  title, and that the comment targets the found issue and carries the run URL — is checked
  in Task 3 by asserting the exact argv against a stubbed `gh`. Only the network effect is
  uncovered.

Note what no mode covers: nothing in `just ci` exercises this file. The repository carries
no actionlint and no yamllint, and the extraction that would have been testable is outside
the permitted surface. Task 3's checks are run by hand and their results recorded in the
spec.

## Task 1 — publish the failing step as a job output

Modifies `.github/workflows/chaos-e2e.yml`.

**Interfaces.** Provides the job output `failing-step` (a single-line step id, or
`unknown-step`), which Task 2 consumes via `needs.chaos-e2e.outputs.failing-step`.

### Step 1.1 — give every step in the `chaos-e2e` job an id

Add an `id:` line directly under each existing `name:` line, in order: `checkout`,
`cache-cargo`, `install-just`, `install-uv`, `install-mkvtoolnix`, `install-ffmpeg`,
`verify-external-tools`, `run-chaos-e2e`. Change nothing else about those steps — not their
`uses:`, `with:`, `env:`, or `run:` bodies.

### Step 1.2 — declare the job output

Between `timeout-minutes: 60` and `steps:`, add:

```yaml
    # Carries the failing step's id to notify-failure, which cannot see this job's
    # steps context. It is the dedupe key for the tracking issue.
    outputs:
      failing-step: ${{ steps.name-failing-step.outputs.name }}
```

### Step 1.3 — add the naming step

Append to the end of the `chaos-e2e` job's `steps:` list, after `Run Chaos Librarian E2E`:

```yaml
      - name: Name the failing step
        id: name-failing-step
        if: failure()
        env:
          STEPS_CONTEXT: ${{ toJSON(steps) }}
          LC_ALL: C
        run: |
          set -euo pipefail
          name=$(printf '%s' "$STEPS_CONTEXT" | jq -r '
            [to_entries[] | select(.value.outcome? == "failure") | .key]
            | first // "unknown-step"')
          if [[ ! $name =~ ^[A-Za-z0-9_-]{1,64}$ ]]; then
            echo "::warning::failing step id is not a plain slug; reporting unknown-step"
            name=unknown-step
          fi
          echo "Failing step: $name"
          printf 'name=%s\n' "$name" >> "$GITHUB_OUTPUT"
```

Three constructs are load-bearing and must not be simplified away. `set -euo pipefail`,
because a `run:` block with no `shell:` key gets `bash -e` **without** `pipefail`, so a
`jq` failure at the head of the pipeline would be invisible. `.value.outcome?`, which
tolerates a non-object entry instead of aborting the filter. `LC_ALL: C`, because the
charset guard is a bracket range and glibc resolves a range through the locale's collation.

**Acceptance criteria.** All eight steps carry ids; the job declares the output; the
naming step runs only on failure; no `permissions:` line changed.

## Task 2 — dedupe the tracking issue

Modifies `.github/workflows/chaos-e2e.yml`.

**Interfaces.** Consumes `needs.chaos-e2e.outputs.failing-step` from Task 1.

### Step 2.1 — replace the notify job's step

Replace the whole `Open tracking issue` step with `Open or update the tracking issue` as
implemented on the branch. Leave the job's `needs:`, `if:`, `runs-on:`, and `permissions:`
lines exactly as they are. Its shape:

1. `set -euo pipefail`, then `step=${FAILING_STEP:-unknown-step}`, warning via
   `::warning::` when the output was absent.
2. Re-check `step` against `^[A-Za-z0-9_-]{1,64}$` on this side of the job boundary,
   warning and degrading to `unknown-step` on a mismatch. The producing job builds and runs
   the whole workspace plus a submodule; this is the job holding `issues: write`, so the
   guard in the producing job is not a control on what this one consumes.
3. `title="Scheduled chaos-e2e run failed: $step"`.
4. Assign `search_json` from `gh issue list --state open --search "in:title \"$title\"
   author:app/github-actions" --limit 50 --json number,title,author` **in an `if !` guard**,
   not through a pipeline. On non-zero, emit `::error::could not search for an existing
   tracking issue; refusing to file a possible duplicate` and `exit 1`.
5. Select with `jq -r --arg title "$title"`, requiring `.title == $title` **and**
   `.author.is_bot? == true`.
6. Found → `gh issue comment "$existing"` naming the run, then `exit 0`.
7. Not found → `gh issue create` with `--title "$title" --label bug --label
   status:needs-triage` and a body carrying the run URL.

Four constructs are load-bearing. The `author:app/github-actions` **server-side** qualifier:
without it anyone can open 50 issues whose titles merely contain the key and push the real
tracking issue out of the `--limit` window, so the exact match finds nothing and a
duplicate is filed every run under their control. The `is_bot` check rather than a login
string: `gh` reports that account as `app/github-actions`, **not** `github-actions[bot]`,
so matching the `[bot]` spelling would never match and every run would file a duplicate.
The exact `.title == $title` comparison, because `in:title` matches by phrase containment.
And the `if !` guard around the search, for the pipefail reason above.

**Acceptance criteria.** The job searches before creating; comments and stops on an
exact-title bot-authored open issue; otherwise creates one carrying both labels; the title
carries the failing step id; `permissions:` unchanged; Task 3 green.

## Task 3 — verify

No file changes. This is the `focused-test` entry for both pure halves.

### Step 3.1 — parse and confirm the grant is untouched

```sh
python3 -c "
import yaml
d=yaml.safe_load(open('.github/workflows/chaos-e2e.yml'))
print('parses OK')
print('outputs:', d['jobs']['chaos-e2e']['outputs'])
print('notify permissions:', d['jobs']['notify-failure']['permissions'])
print('ids:', [s.get('id') for s in d['jobs']['chaos-e2e']['steps']])
"
git diff -U0 -- .github/workflows/chaos-e2e.yml \
  | grep -E '^[+-][[:space:]]*(permissions:|issues:|contents:)[[:space:]]*[a-z]*$'
```

Expect `parses OK`, `notify permissions: {'issues': 'write'}`, nine non-null ids, and the
`grep` to print nothing and exit 1.

### Step 3.2 — drive both `run:` blocks against fixtures

Extract each block from the YAML with `yaml.safe_load` into a scratch directory outside the
repository, then run them.

Naming block, with `GITHUB_OUTPUT=/dev/null LC_ALL=C` and `STEPS_CONTEXT` set:

| Context | Expected stdout |
|---|---|
| `run-chaos-e2e` failing, others `success` | `Failing step: run-chaos-e2e` |
| `install-ffmpeg` failing, `run-chaos-e2e` `skipped` | `Failing step: install-ffmpeg` |
| all `success` | `Failing step: unknown-step` |
| only `cancelled`/`skipped` | `Failing step: unknown-step` |
| key `run "$(id)" e2e` failing | `::warning::` then `Failing step: unknown-step` |
| a non-object entry beside a real failure | `Failing step: run-chaos-e2e` |

Notify block, with a stub `gh` on `PATH` that logs its argv and returns a scripted
`issue list` response:

| Case | Expected |
|---|---|
| search exits non-zero | `::error::…refusing to file a possible duplicate`, exit 1, **no `create`** |
| exact title, `is_bot` true | `gh issue comment 700`, no `create` |
| title matches only by containment | falls through to `create` |
| exact title, `is_bot` false | falls through to `create` |
| empty result | `create` carrying `--label bug` and `--label status:needs-triage` |
| `FAILING_STEP` empty | `::warning::chaos-e2e published no failing-step output…` |
| `FAILING_STEP='run "$(id)" e2e'` | `::warning::rejected a malformed failing-step value…` |

### Step 3.3 — run the search pipeline against the live repository

Read-only, no runner, no permission change:

```sh
for title in "Scheduled chaos-e2e run failed" "Scheduled chaos-e2e run failed: run-chaos-e2e"; do
  for state in all open; do
    gh issue list --repo randomparity/voom-v2 --state "$state" \
      --search "in:title \"$title\" author:app/github-actions" --limit 50 \
      --json number,title,author \
      | jq -r --arg title "$title" \
          '[ .[] | select(.title == $title and .author.is_bot? == true) | .number ] | first // "(none)"'
  done
done
```

Expect `536` for the old key under `--state all` and `(none)` for the other three: the old
duplicates are closed, so the first scheduled failure after merge opens one issue under the
new key, and the new key does not false-match by containment.

### Step 3.4 — guardrails and commit

Run `just ci` bare. Expect exit 0 and `==> All CI checks passed`.

## Review state

Design review: `$gauntlet` via `$trial-loop`, 1 cycle, budget 2, 2 rounds. Iteration 1 — 6
findings, 1 blocking, fixed. Iteration 2 — 8 findings, 1 blocking, 7 notes; the notes were
all `accepted-fixed` and the blocking finding (requirement 4's test half left unstated) was
escalated to the operator, who chose to accept the step-id key and have the judgement
stated. That is now *The test half of requirement 4* in the spec.

No `deferred-tracked` findings, so no `docs/debt/` record. One `rejected-with-evidence`:
adding an `actionlint` hook over `.github/workflows/*.yml` to the guardrail suite. The
concern is real and is recorded in the spec's *Testing* section — nothing statically checks
this workflow's Actions schema or its `needs.*.outputs` expression — but the remedy adds a
third-party linter to the repository guardrail suite, outside this change's surface, and
would gate the two workflows this issue excludes. Reported as follow-up work.
