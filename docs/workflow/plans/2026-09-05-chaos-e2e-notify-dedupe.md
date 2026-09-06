# chaos-e2e notify dedupe — implementation plan

Goal: make the `notify-failure` job in `.github/workflows/chaos-e2e.yml` reuse one open
tracking issue per distinct failure instead of filing a new one every failed scheduled run.

Architecture: the `chaos-e2e` job gains a step `id:` on every step and a final
`if: failure()` step that pipes `toJSON(steps)` through a new
`scripts/name-failing-step.sh`, publishing the failing step's id as the job output
`failing-step`. The `notify-failure` job reads that output, builds the dedupe key
`Scheduled chaos-e2e run failed: <step>`, searches open issues for an exact title match
authored by a bot, and either comments on the match or creates the issue with `bug` and
`status:needs-triage`.

Tech stack: GitHub Actions workflow YAML, bash, `jq`, `gh`, `just`, `prek`.

Spec: `docs/workflow/specs/2026-09-05-chaos-e2e-notify-dedupe-design.md`.

Expected implementation size: 180–240 changed lines (M) — derived from the file map below: one new ~50-line script, one new ~130-line selftest, and ~55 changed lines across the workflow, `justfile`, and `.pre-commit-config.yaml`.

## Global Constraints

- Base branch `main`; branch `feat/dedupe-chaos-notify-496`.
- Guardrail command: `just ci`. It is the exact CI suite — CI invokes `just ci` directly,
  so every sub-check hard-gates the PR. Run it bare: no `| tail`, no `>/dev/null`, no
  `|| true`.
- The `prek` pre-push hook re-runs the suite in an isolated worktree and is slow. That is
  slowness, not a hang; do not re-invoke it after an apparent timeout.
- **No `permissions:` block in any workflow may change.** Completion criterion 5 of the
  frozen scope charter on issue #496 forbids widening the `notify-failure` grant, which is
  and stays exactly `issues: write`.
- Shell scripts under `scripts/` are bash, start `#!/usr/bin/env bash`, set
  `set -euo pipefail`, indent with tabs, and are `chmod +x`. Follow
  `scripts/select-ffmpeg-asset.sh` and `scripts/select-ffmpeg-asset-selftest.sh`, the
  existing extraction from this same workflow file.
- `jq` is an existing documented local dependency (`scripts/chaos-e2e-local.sh` requires
  it) and is preinstalled on `ubuntu-latest`.
- Repository labels `bug` and `status:needs-triage` both exist and are used verbatim.
- Do not touch `.github/workflows/constrained-resources.yml` or
  `.github/workflows/net-resilience.yml`. They carry the identical defect and are
  explicitly excluded from this change.
- No ADR, and therefore no `docs/adr/README.md` row.

## File map

| File | Status | Answerable for |
|---|---|---|
| `scripts/name-failing-step.sh` | new | Deriving the failing step's id from a steps context on stdin, and bounding its charset. |
| `scripts/name-failing-step-selftest.sh` | new | Pinning one rule of that derivation per case. |
| `justfile` | modified | The `name-failing-step-selftest` recipe and its entry in `ci:`. |
| `.pre-commit-config.yaml` | modified | Running that recipe when the script or the justfile changes. |
| `.github/workflows/chaos-e2e.yml` | modified | Step ids, the `failing-step` job output, and the dedupe logic in `notify-failure`. |

## Verification inventory

Two material contracts change.

- **Failing-step derivation** (`scripts/name-failing-step.sh`).
  `Mode: focused-test`. Observable contract: given a `toJSON(steps)` object on stdin, the
  script writes the id of the first entry whose `outcome` is `failure`, writes
  `unknown-step` when no entry qualifies or the id leaves `^[A-Za-z0-9_-]{1,64}$`, and
  exits non-zero on absent or non-object input. Test file:
  `scripts/name-failing-step-selftest.sh`. Expected red before the script exists: the
  selftest aborts because `scripts/name-failing-step.sh` is not executable. Green command:
  `just name-failing-step-selftest`, expected final line
  `name-failing-step-selftest: OK`.
- **The `notify-failure` job body** (`.github/workflows/chaos-e2e.yml`).
  `Mode: task-test-not-applicable`. Changed surface: the search-then-comment-or-create
  sequence. Reason: the sequence is `gh` calls against the live GitHub Issues API executed
  inside a job that carries no repository checkout, so no in-repo harness can invoke it;
  giving it one requires adding `contents: read` to that job's `permissions:` block, which
  replaces the workflow default rather than adding to it, and Global Constraints forbids
  that change. This is a knowing gap, not a categorical one — it is why the derivation
  above was extracted into a script that *can* be tested, and it is reported rather than
  papered over.

## Task 1 — extract and test the failing-step derivation

Creates `scripts/name-failing-step.sh` and `scripts/name-failing-step-selftest.sh`;
modifies `justfile` and `.pre-commit-config.yaml`.

**Interfaces.** Consumes nothing from earlier tasks. Task 2 relies on this exact contract:

    scripts/name-failing-step.sh          # no arguments; steps-context JSON on stdin
    # stdout: <step-id> | unknown-step    (single line, trailing newline)
    # exit 0  a name was written
    # exit 2  arguments were passed
    # exit 3  stdin was absent, unparseable, or not a JSON object

Where it fits: this is the whole of the logic the workflow cannot test, pulled out so it
can be. Task 2 calls it from the `chaos-e2e` job, which already checks the repo out.

### Step 1.1 — write the failing test first

Create `scripts/name-failing-step-selftest.sh` with exactly this content, then
`chmod +x scripts/name-failing-step-selftest.sh`.

```bash
#!/usr/bin/env bash
# Self-test for scripts/name-failing-step.sh.
#
# Each case pins one rule of the derivation. Break a rule and exactly one case
# should redden.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
name_script="$script_dir/name-failing-step.sh"
task_tmp=$(mktemp -d "${TMPDIR:-/tmp}/voom-name-failing-step-selftest.XXXXXX")

cleanup() {
	rm -f "$task_tmp"/*
	rmdir "$task_tmp"
}
trap cleanup EXIT

failures=0

# Assert stdout and exit status.
#   expect_ok <case> <expected-stdout> < <steps-json>
expect_ok() {
	local case_name=$1 expected=$2 actual status
	set +e
	actual=$("$name_script" 2>"$task_tmp/err")
	status=$?
	set -e
	if ((status != 0)); then
		echo "name-failing-step-selftest: $case_name: expected exit 0, got $status" >&2
		cat "$task_tmp/err" >&2
		failures=$((failures + 1))
		return
	fi
	if [[ $actual != "$expected" ]]; then
		echo "name-failing-step-selftest: $case_name: expected '$expected', got '$actual'" >&2
		failures=$((failures + 1))
	fi
}

# Assert exit status AND a substring of stderr. Without the substring, swapping
# the exit-2 and exit-3 messages would stay green.
#   expect_stderr <case> <expected-status> <substring> <args...> < <steps-json>
expect_stderr() {
	local case_name=$1 expected=$2 needle=$3
	shift 3
	local status
	set +e
	"$name_script" "$@" >"$task_tmp/out" 2>"$task_tmp/err"
	status=$?
	set -e
	if ((status != expected)); then
		echo "name-failing-step-selftest: $case_name: expected exit $expected, got $status" >&2
		cat "$task_tmp/err" >&2
		failures=$((failures + 1))
		return
	fi
	if ! grep -qF -- "$needle" "$task_tmp/err"; then
		echo "name-failing-step-selftest: $case_name: stderr lacks '$needle'" >&2
		cat "$task_tmp/err" >&2
		failures=$((failures + 1))
	fi
}

# `toJSON(steps)` as Actions emits it for this job: keyed by step id, every entry
# carrying outputs/outcome/conclusion. Kept whole on purpose -- the seven
# successful entries are what prove the selection discriminates on outcome
# rather than picking the first or the last key.
context() {
	cat <<-'STEPS'
	{
	  "checkout":              {"outputs": {}, "outcome": "success", "conclusion": "success"},
	  "cache-cargo":           {"outputs": {}, "outcome": "success", "conclusion": "success"},
	  "install-just":          {"outputs": {}, "outcome": "success", "conclusion": "success"},
	  "install-uv":            {"outputs": {}, "outcome": "success", "conclusion": "success"},
	  "install-mkvtoolnix":    {"outputs": {}, "outcome": "success", "conclusion": "success"},
	  "install-ffmpeg":        {"outputs": {}, "outcome": "success", "conclusion": "success"},
	  "verify-external-tools": {"outputs": {}, "outcome": "success", "conclusion": "success"},
	  "run-chaos-e2e":         {"outputs": {}, "outcome": "failure", "conclusion": "failure"}
	}
	STEPS
}

expect_ok "full context" "run-chaos-e2e" < <(context)

# A setup failure leaves the later steps skipped. This is the case that must not
# report run-chaos-e2e -- the whole point of keying the issue on the step.
expect_ok "setup failure" "install-ffmpeg" <<-'EOF'
	{
	  "checkout":       {"outcome": "success"},
	  "install-ffmpeg": {"outcome": "failure"},
	  "run-chaos-e2e":  {"outcome": "skipped"}
	}
EOF

# `skipped` and `cancelled` are not `failure`. A filter written as
# `!= "success"` passes every other case and fails only this one.
expect_ok "no failure among non-successes" "unknown-step" <<-'EOF'
	{
	  "checkout":      {"outcome": "success"},
	  "install-uv":    {"outcome": "skipped"},
	  "run-chaos-e2e": {"outcome": "cancelled"}
	}
EOF

# Two failures can only arise from a continue-on-error step; the earlier one is
# the cause, so the filter takes the first rather than the last.
expect_ok "first failure wins" "install-ffmpeg" <<-'EOF'
	{
	  "install-ffmpeg": {"outcome": "failure"},
	  "run-chaos-e2e":  {"outcome": "failure"}
	}
EOF

# A caller always gets a usable key back, so the notify job never has to invent one.
expect_ok "no failure at all" "unknown-step" <<-'EOF'
	{"checkout": {"outcome": "success"}}
EOF
expect_ok "empty context" "unknown-step" <<-'EOF'
	{}
EOF

# A non-object entry must not abort the filter; the real failure is still named.
# `.value.outcome?` is the only construct that makes this pass.
expect_ok "non-object entry tolerated" "run-chaos-e2e" <<-'EOF'
	{
	  "checkout":      "success",
	  "run-chaos-e2e": {"outcome": "failure"}
	}
EOF

# The charset boundary. This key carries quotes, spaces and a command
# substitution; none of it may reach an issue title or a search query.
expect_ok "hostile key degraded" "unknown-step" <<-'EOF'
	{"run \"$(id)\" e2e": {"outcome": "failure"}}
EOF

# ... and the degrade is announced, so it is never silent.
expect_stderr "hostile key announced" 0 "is not a plain step id" <<-'EOF'
	{"run \"$(id)\" e2e": {"outcome": "failure"}}
EOF

# Both sides of the length bound: with only one, the bound is untested.
expect_ok "64-character key accepted" "$(printf 'a%.0s' {1..64})" \
	<<<"{\"$(printf 'a%.0s' {1..64})\": {\"outcome\": \"failure\"}}"
expect_ok "65-character key degraded" "unknown-step" \
	<<<"{\"$(printf 'a%.0s' {1..65})\": {\"outcome\": \"failure\"}}"

# Valid JSON that is not an object is a caller bug, not a no-failure result: it
# must exit 3, never print `unknown-step` and exit 0.
expect_stderr "array context" 3 "could not read a steps context" <<-'EOF'
	["run-chaos-e2e"]
EOF
expect_stderr "null context" 3 "could not read a steps context" <<-'EOF'
	null
EOF
expect_stderr "malformed json" 3 "could not read a steps context" <<-'EOF'
	{"run-chaos-e2e":
EOF

# An absent context is distinguished from an empty one: `{}` is a real answer,
# nothing at all is not. The here-string carries real spaces; <<- strips tabs.
expect_stderr "empty stdin" 3 "no steps context on stdin" </dev/null
expect_stderr "whitespace-only stdin" 3 "no steps context on stdin" <<<'   '

# The script reads stdin and takes no arguments.
expect_stderr "one argument" 2 "usage" run-chaos-e2e </dev/null

if ((failures > 0)); then
	echo "name-failing-step-selftest: $failures case(s) failed" >&2
	exit 1
fi

echo "name-failing-step-selftest: OK"
```

### Step 1.2 — confirm the expected failure

Run `./scripts/name-failing-step-selftest.sh`. Expect a non-zero exit with a
"No such file or directory" error naming `scripts/name-failing-step.sh`: the script under
test does not exist yet.

### Step 1.3 — write the script

Create `scripts/name-failing-step.sh` with exactly this content, then
`chmod +x scripts/name-failing-step.sh`.

```bash
#!/usr/bin/env bash
# Name the step that failed in the calling job, for use as a tracking-issue
# dedupe key.
#
# Reads a GitHub Actions `steps` context as JSON on stdin -- the object
# `toJSON(steps)` produces, keyed by step id -- and writes the id of the first
# step whose outcome is "failure" to stdout, or `unknown-step` when the context
# names none. See docs/workflow/specs/2026-09-05-chaos-e2e-notify-dedupe-design.md.
#
# The id is the key rather than the display name because the context carries only
# ids: a step added to the job with an `id:` is covered here with no second list
# of names to keep in sync.
set -euo pipefail
# The name reaches an issue title and a `gh` search query, so the charset check
# below must not shift with the runner's locale. scripts/select-ffmpeg-asset.sh
# documents why the export rather than the character class is the portable guard.
export LC_ALL=C

if (($# != 0)); then
	echo "name-failing-step: usage: name-failing-step.sh < <steps-context-json>" >&2
	exit 2
fi

steps_json=$(cat)

# An absent context is not an empty one: `{}` is a real answer from a job with no
# failed step, whereas nothing at all means the caller did not pipe the context in.
if [[ -z ${steps_json//[[:space:]]/} ]]; then
	echo "name-failing-step: no steps context on stdin" >&2
	exit 3
fi

# `.value.outcome?` tolerates a non-object entry instead of aborting the filter.
# `first // "unknown-step"`: `first` of an empty array is null, and null // x is x.
if ! name=$(jq -r '
	if type != "object" then error("steps context is not an object") else . end
	| [to_entries[] | select(.value.outcome? == "failure") | .key]
	| first // "unknown-step"
' <<<"$steps_json"); then
	echo "name-failing-step: could not read a steps context from stdin" >&2
	exit 3
fi

# Actions already constrains a step id to this shape, so this is a boundary check
# rather than a transformation: degrade rather than let a quote, a space or a `$`
# reach an issue title. Losing the suffix beats losing the notification.
if [[ ! $name =~ ^[A-Za-z0-9_-]{1,64}$ ]]; then
	echo "name-failing-step: '$name' is not a plain step id, reporting unknown-step" >&2
	name=unknown-step
fi

printf '%s\n' "$name"
```

### Step 1.4 — confirm the expected pass

Run `./scripts/name-failing-step-selftest.sh`. Expect exit 0 and the final line
`name-failing-step-selftest: OK`.

### Step 1.5 — wire the recipe into `just ci`

In `justfile`, add this recipe immediately after the `select-ffmpeg-asset-selftest`
recipe:

```just
# Self-test for the failing-step namer (keeps the chaos-e2e dedupe key honest)
name-failing-step-selftest:
    ./scripts/name-failing-step-selftest.sh
```

Then add it to the `ci:` recipe's dependency list, on the line that already carries
`select-ffmpeg-asset-selftest`, so that line reads:

```just
    check-adr-index check-adr-index-selftest select-ffmpeg-asset-selftest \
    name-failing-step-selftest \
```

### Step 1.6 — wire the prek hook

In `.pre-commit-config.yaml`, add this entry immediately after the
`select-ffmpeg-asset-selftest` hook, matching its shape:

```yaml
      - id: name-failing-step-selftest
        name: just name-failing-step-selftest
        entry: just name-failing-step-selftest
        language: system
        pass_filenames: false
        files: '^(scripts/name-failing-step.*\.sh|justfile)$'
```

### Step 1.7 — verify and commit

Run `just name-failing-step-selftest`. Expect exit 0 and `name-failing-step-selftest: OK`.

Commit with `feat(ci): name the failing chaos-e2e step for issue dedupe`.

**Acceptance criteria.** `scripts/name-failing-step.sh` and its selftest exist and are
executable; `just name-failing-step-selftest` passes; the recipe appears in `ci:`; the prek
hook entry matches the `select-ffmpeg-asset-selftest` shape; no `permissions:` block
changed.

## Task 2 — dedupe the tracking issue

Modifies `.github/workflows/chaos-e2e.yml` only.

**Interfaces.** Consumes `scripts/name-failing-step.sh` from Task 1, at the exact contract
quoted there. Provides no interface to a later task; this is the last task.

Where it fits: this is the behaviour change the issue asks for. Task 1 supplied the only
part of it that is testable off a runner.

### Step 2.1 — give every step in the `chaos-e2e` job an id

In the `chaos-e2e` job, add an `id:` line directly under each existing `name:` line, in
order:

| Existing `name:` | `id:` to add |
|---|---|
| `Checkout` | `checkout` |
| `Cache cargo` | `cache-cargo` |
| `Install just` | `install-just` |
| `Install uv` | `install-uv` |
| `Install MKVToolNix` | `install-mkvtoolnix` |
| `Install ffmpeg` | `install-ffmpeg` |
| `Verify external tools` | `verify-external-tools` |
| `Run Chaos Librarian E2E` | `run-chaos-e2e` |

Change nothing else about those steps — not their `uses:`, `with:`, `env:`, or `run:`
bodies.

### Step 2.2 — publish the failing step as a job output

In the `chaos-e2e` job, between `timeout-minutes: 60` and `steps:`, add:

```yaml
    outputs:
      failing-step: ${{ steps.name-failing-step.outputs.name }}
```

### Step 2.3 — add the naming step

Append this step to the end of the `chaos-e2e` job's `steps:` list, after
`Run Chaos Librarian E2E`:

```yaml
      # Names the step that failed, so notify-failure can key one tracking issue
      # per distinct failure. Keyed by step id: a step added above with an `id:`
      # is covered with nothing here to update. The derivation is a script rather
      # than an inline filter because it is the part with a silent failure mode --
      # a wrong filter reports `unknown-step` for everything and recollapses every
      # failure into one issue. `just name-failing-step-selftest` is what holds it.
      - name: Name the failing step
        id: name-failing-step
        if: failure()
        env:
          STEPS_CONTEXT: ${{ toJSON(steps) }}
        run: |
          name=$(printf '%s' "$STEPS_CONTEXT" | ./scripts/name-failing-step.sh)
          echo "Failing step: $name"
          printf 'name=%s\n' "$name" >> "$GITHUB_OUTPUT"
```

### Step 2.4 — rewrite the notify job's step

Replace the whole `Open tracking issue` step in the `notify-failure` job with:

```yaml
      - name: Open or update the tracking issue
        env:
          GH_TOKEN: ${{ github.token }}
          RUN_URL: ${{ github.server_url }}/${{ github.repository }}/actions/runs/${{ github.run_id }}
          # Empty when the job died before its naming step ran. A vague key beats
          # no notification, so this falls back rather than failing.
          FAILING_STEP: ${{ needs.chaos-e2e.outputs.failing-step }}
        run: |
          step=${FAILING_STEP:-unknown-step}
          title="Scheduled chaos-e2e run failed: $step"

          # `in:title` matches by phrase containment, not equality, so the search
          # only narrows the candidates -- the exact title comparison is the rule.
          # `is_bot` is what stops an outside user capturing this notification
          # stream by opening an issue under the predictable title; gh reports the
          # Actions account as `app/github-actions`, not `github-actions[bot]`.
          existing=$(gh issue list \
            --repo "$GITHUB_REPOSITORY" \
            --state open \
            --search "in:title \"$title\"" \
            --limit 50 \
            --json number,title,author \
            | jq -r --arg title "$title" '
                [ .[]
                  | select(.title == $title and .author.is_bot? == true)
                  | .number
                ] | first // empty')

          if [[ -n $existing ]]; then
            echo "Reusing tracking issue #$existing"
            gh issue comment "$existing" \
              --repo "$GITHUB_REPOSITORY" \
              --body "The scheduled chaos-e2e workflow failed again at \`$step\`. Run: $RUN_URL"
            exit 0
          fi

          echo "No open tracking issue titled '$title'; creating one"
          body=$(printf '%s\n\n%s\n\n%s' \
            "The weekly scheduled chaos-e2e workflow failed at \`$step\`." \
            "Run: $RUN_URL" \
            "This issue is reused for later failures of the same step; each failed run adds a comment.")
          gh issue create \
            --repo "$GITHUB_REPOSITORY" \
            --title "$title" \
            --label bug \
            --label status:needs-triage \
            --body "$body"
```

Leave the job's `needs:`, `if:`, `runs-on:`, and `permissions:` lines exactly as they are.

### Step 2.5 — verify

Confirm the workflow still parses and that the permission grant is untouched:

```sh
python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/chaos-e2e.yml'))" \
  && echo "chaos-e2e.yml parses"
git diff -- .github/workflows/chaos-e2e.yml | grep -E '^[+-].*permissions|^[+-].*issues: write'
```

Expect `chaos-e2e.yml parses`, and expect the `grep` to print nothing and exit 1 — no
permission line was added, removed, or changed.

### Step 2.6 — run the guardrail suite and commit

Run `just ci` bare. Expect exit 0 and the final line `==> All CI checks passed`.

Commit with `fix(ci): reuse one chaos-e2e tracking issue per failing step`.

**Acceptance criteria.** The notify job searches before creating; comments and stops when
an exact-title bot-authored open issue exists; otherwise creates one carrying `bug` and
`status:needs-triage`; the title carries the failing step id; `permissions:` is unchanged;
`just ci` is green.

## Review deferrals

None yet. Deferrals from the design review and the branch review are recorded here as they
are dispositioned.
