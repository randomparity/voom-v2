# GitHub Actions static checks (#621)

## Problem and scope

The six `.github/workflows/*.yml` files have no static gate. CI calls
`just source-checks` and `just platform-checks`, so extending only an unused
umbrella would not protect pull requests. The approved scope is #621's analyzers,
provisioning, and first-run findings. Notification deduplication (#620), sharding
(#641), broad hook runtime (#485), and unrelated code remain separate.

## Decision

Use actionlint 1.7.12 with mandatory ShellCheck 0.11.0, plus zizmor 1.30.1's
regular offline audits. The tools cover complementary schema/expression, shell,
and injection/security concerns. These are current official stable releases.
Offline checks run without secrets or GitHub API access; online-only audits
are outside this static gate. Do not filter severities or disable exit codes.

A shared `just check-workflows` recipe checks the six workflow files and fails
with an actionable `just setup-workflow-tools` remedy when tools are missing.
`source-checks` and the workflow-specific prek hook call that recipe. Provisioning
uses official release archives with pinned SHA256 digests on macOS/Linux arm64
and x86_64, under ignored `target/workflow-tools/`. It installs no new compiler
or package manager. `just setup` and CI's source jobs call the same setup recipe.
Downloads are verified before extraction; temporary state is cleaned on exit;
installation stages each executable before replacing its destination.

## First-run findings

The first actionlint run passed. Zizmor 1.30.1 exited 14 with eight active findings:
- Five unpinned setup-just inputs: pin current stable 1.58.0.
- Release tag template injection: consume `GITHUB_REF_NAME` as quoted shell data.
- Release cache poisoning: remove the release job's Rust cache restoration.
- Informational superfluous release action: retain it with one documented inline
  suppression because the existing action updates the same draft across matrix jobs;
  replacing its release coordination is unnecessary for this guard.
The tool's regular persona suppresses 21 additional lower-confidence/pedantic
observations by default. No new blanket ignore or repository config is added.

## Success

`just check-workflows` and `just source-checks` reject malformed workflow schema,
ShellCheck defects, and untrusted expression injection in the top-level workflow
set. A clean set succeeds. The named source CI jobs execute the same guard on Linux
and macOS. A developer can provision the exact analyzer versions without Go/Rust
floor changes. Existing release targets, draft behavior, and token permissions stay
unchanged. First-run fixes remove seven active findings and justify the eighth.

## Failure model

- Actors and deployments: local developers and GitHub-hosted Linux/macOS source
  jobs, on arm64/x86_64; workflow changes from contributors are untrusted inputs.
- Invariants and assets: missing tools must not silently skip checks; rejected
  downloads are never executed; the workflow gate fails on analyzer failures;
  release credentials and artifacts remain protected from expression injection.
- Accepted failure classes: analyzer false negatives are inherent to static
  analysis, which does not prove live service permissions or runtime correctness;
  offline mode omits network-dependent audits to keep local/CI runs credential-free;
  upstream release compromise requires digest updates and remains a supply-chain risk.
- Covered elsewhere: production Rust behavior by existing CI; notification behavior
  by #620; sharding by #641; broad hook runtime by #485.

## Threat model

- Added boundary: official release archives enter a locally executable tool directory.
  Trust repository-reviewed URL/digest pins, not network content. SHA256 verification
  precedes extracting only the named executable; failures disclose tool names, not secrets.
- Existing boundary narrowed: a tag author controls `GITHUB_REF_NAME`; quoted shell
  expansion keeps the tag as data. Workflow contributors control analyzer inputs;
  analyzers parse those inputs without executing their run blocks.
- Actors: contributors and tag authors are input producers; repository maintainers
  approve pins and tool code. Provisioning accepts no arbitrary download URL.
- Out of scope: malicious approved upstream tool code and static false negatives
  retain the accepted risks above; online service audits remain separate.

## Validation

Run a self-test with safe, invalid-needs, shell defect, and injection workflows;
assert each detector fails on its own relevant fixture. Exercise missing-tool
failure and download checksum rejection without writing user tool locations.
Run the real analyzer set, full source guards, native applicable hooks, and one
final `just ci`; record first-run output. Hosted Linux/macOS source checks prove
both provisioning paths. Release packaging receives a hostile-tag shell control;
no live release is published for testing.
