# HTTP-safe scan child identities (#731)

## Problem
The unchanged authenticated stack scans a synthetic ASCII filename successfully,
but the same file named `é.mkv` reproduces the reported missing-idempotency error.
A transparent diagnostic proxy captured a present header containing UTF-8 bytes.
The protocol server deliberately accepts only values convertible with `to_str()`;
non-ASCII locator bytes therefore fail its existing nonempty-header check.
`process_candidate` currently embeds primary and sidecar locators in child keys.

## Scope and authority
Issue #731, the later explicit inventory request and re-frozen `WORK:SCOPE`
comment 6373240608 (token `q731-61f553cb`) authorize a demonstrated
producer repair and direct tests. Preserve ADR0067 lifecycle/fences and ADR0050
owner-byte responsibility. No ownership transition or caller migration is needed:
the node-agent pump owns dispatch identity and continues calling existing clients.
Do not alter protocol validation, cache replay, schema, authentication or the DSL.
#339 retains acceptance; the operator retains media/retention/full-run decisions.

## Design
Replace only the locator components of primary and sidecar dispatch tags with
lowercase BLAKE3 hex digests of their exact UTF-8 bytes. The existing incarnation,
lease and operation suffixes remain. The existing BLAKE3 dependency suffices.
Primary and sidecar digests occupy separate components, preserving their roles;
request payloads retain the original locators. Same execution/locator/operation
retries retain keys; another lease or locator yields a different key.

The protocol remains strict. Encoding is the producer's responsibility: accepting
arbitrary bytes server-side would weaken the existing contract. Hex-encoding full
locators is avoidable key growth; fixed digests bound the locator contribution and
avoid exposing names in keys. No new durable format, migration or ADR is required.

## Inventory selection support
The operator additionally requested path diversity in the existing interesting-media
inventory. Derive deterministic tags from the root-relative locator, not the host
root: non-ASCII characters anywhere, any component at least 200 UTF-8 bytes,
relative path at least 1024 UTF-8 bytes, leading-dash components, and ASCII control
characters (U+0000–001F/U+007F). The thresholds are sampling heuristics near common
255-byte component and multi-component path pressure, not filesystem validity rules.
Do not normalize or rename paths. Common spaces/shell punctuation and Unicode
normalization forms need no separate category: argv remains literal and exact
locator bytes already distinguish names.

Attach tags to successful and failed-probe records. Add path priorities before
broad media/compatibility priorities in both existing profiles; reference-user
still excludes incompatible inputs. Extend the existing size/basename dedup key
with the path-feature tuple so a non-ASCII parent variant survives an otherwise
identical basename/size. Do not abandon equivalent-media dedup within a feature
class. Focused selftests must prove recommendation membership and byte boundaries,
root-prefix isolation, failed-probe retention and unchanged compatibility filtering.
Wire them into `just source-checks`, which both CI source jobs already execute.

## Failure model
Accepted deployments are authenticated bundled software workers on supported
hosts, with UTF-8 locators already accepted by existing scan contracts.
- Non-ASCII, whitespace and delimiter-rich names must produce ASCII keys.
- Distinct primary/sidecar identities must not alias by delimiter concatenation.
- Dispatch rejection must still fail the ticket and surface a terminal scan state;
  the reduced real failure does settle the session, so no settlement rewrite is
  justified. The earlier running-session observation remains a disclosed unknown.
- Cryptographic digest collision is the existing BLAKE3 integrity assumption;
  arbitrary non-UTF-8 filesystem names remain outside the existing locator contract.

## Success
1. Primary and sidecar keys are bounded ASCII and preserve replay/isolation intent.
2. Payload locators and all protocol guards remain unchanged.
3. Real rooted Unicode scan and repeated scan succeed with expected observations,
   successful tickets and consumed access plans; rejected child dispatch still
   returns a failed scan without publishing/retiring locations.
4. Inventory recommendations preserve path edge cases in both modes even when
   codec facts match; failed probes retain path tags and root prefixes never tag.
5. Focused regressions, repository guards and independent review pass.

## Validation
The implementation plan inventories the changed key contract. Capture the focused
regression red before repair, then green; retain diagnostic real-stack negative
and positive evidence outside Git. Run `just ci` before push. No production or
canary execution belongs to this prerequisite.
