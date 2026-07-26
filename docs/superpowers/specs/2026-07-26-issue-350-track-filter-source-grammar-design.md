# Issue 350 design: Enforce the published track-filter source grammar

## Objective

Make every filter-consuming policy operation accept exactly the track-filter
grammar published in `docs/specs/voom-control-plane-design.md`. Reject aliases,
unquoted token values, parser-only predicates and comparators, malformed
expressions, and trailing input before lowering, without changing the durable
compiled-policy contract.

## Scope and ownership

In scope:

- source validation for every published track-filter leaf;
- Boolean `not`, `and`, `or`, grouping, and precedence;
- every filter consumer: `keep`, `remove`, `transcode audio`, `extract audio`,
  `defaults ... where`, `order tracks ... where`, and `synthesize audio from`;
- canonical source fixtures and current operator examples;
- compiler tests that prove accepted and rejected source forms; and
- deserialization evidence for historical compiled `TrackFilter` variants.

Explicitly excluded:

- selector execution semantics owned by #331 and #332;
- `defaults ... best` selection owned by #336;
- general condition grammar outside track filters;
- parser AST permissiveness;
- broad compiled-policy JSON strictness owned by #344; and
- any new DSL production, alias, comparator, predicate, or compatibility shim.

An excluded concern returns to scope only if this change depends on it or makes
it worse.

## Governing source contract

The only accepted track-filter leaves are:

```text
language == <quoted-token>
language in [<quoted-token>, ...]
codec in [<quoted-token>, ...]
channels ==|!=|<|<=|>|>= <unsigned ASCII integer>
commentary
forced
default
font
title contains <quoted-string>
```

Leaves compose through `not`, `and`, `or`, and balanced parentheses. Existing
precedence remains `not`, then `and`, then `or`.

A quoted token decodes to one non-empty stable token containing only lowercase
ASCII letters, ASCII digits, `_`, or `-`. It cannot contain whitespace, commas,
quotes, backslashes, or escapes. Language tokens retain the existing semantic
restriction to `und` or a three-letter lowercase ASCII code. Codec tokens
remain domain values interpreted by planning; #350 requires their quotes and
stable-token shape but does not add a codec allowlist.

`title contains` requires one complete, non-empty quoted string. Its scanner
decodes `\"` to `"` and `\\` to `\`; every other escape, a trailing backslash,
or an unclosed string is invalid. The decoded string is the value persisted in
`TrackFilter::TitleContains`.

The following are not source grammar:

- `lang`;
- bare values such as `language == eng`, `language in [eng]`, or
  `codec in [aac]`;
- `title matches`;
- channel comparators `=`, `contains`, or `matches`;
- empty, unclosed, double-comma, or trailing-comma lists;
- missing Boolean operands, unbalanced grouping, or empty grouping; and
- any tokens after a complete leaf, list, string, group, or expression.

The general source parser remains capable of representing raw statements that
contain those spellings. Parsing establishes structure; validation determines
whether a source program is published and compilable. This distinction lets
parser tests continue to exercise generic raw-statement handling without
publishing parser spellings.

## Validation and lowering boundary

`voom-policy` replaces its separate validation recognizer and lowering parser
with one recursive source parser returning `Option<TrackFilter>`. Validation
uses success as its grammar decision and recursively applies the existing
language-code check to parsed `LanguageIn` values. Lowering calls the same
parser after validation. This makes “accepted implies lowerable” structural,
rather than an assumption between two similar parsers. The parser must consume
the complete supplied filter text. Each leaf check is grammar-specific:

- language equality requires the exact `language ==` prefix and one quoted
  token;
- language and codec lists require brackets, at least one quoted element,
  commas between elements, no empty elements, and no text after the closing
  bracket;
- channels accepts exactly three lexical units, one of the six published
  comparators, and an ASCII `u64`;
- fieldless predicates accept exactly one lexical unit; and
- `title contains` requires one decoded quoted string with no trailing input.

The quoted-token list reader is quote-aware and delimiter-aware; it never uses
a generic comma split. The quoted-string reader and decoder is shared by
validation and lowering, so an accepted escape cannot acquire different
compiled semantics.

Boolean splitting continues to occur only outside quoted strings and nested
parentheses. A split is valid only when every child is non-empty and valid.
Stripping an outer group is allowed only when that group is balanced and owns
the complete expression.

Every filter-consuming operation routes its filter through the shared parser;
#350 adds table-driven coverage proving none bypass it. Validation errors
continue to use `unknown_phase_statement_or_operation` and the existing
actionable message `unknown track filter predicate`; no public error code
changes.

The shared parser has no branches for unpublished source (`lang` and
`title matches`). Keep the durable `TrackFilter::TitleMatches` variant because
stored historical compiled policy JSON may contain it. General comparison
lowering remains unchanged because non-track condition compatibility is
outside #350.

## Durable compatibility

#350 changes no compiled type, enum discriminator, field, schema version, serde
annotation, database row, or migration. In particular:

- `TrackFilter::TitleMatches` remains deserializable;
- canonical `LanguageIn`, `CodecIn`, `Channels`, predicate, and Boolean
  variants remain unchanged;
- source aliases are not recompiled or normalized; they are rejected; and
- existing stored compiled policy versions bypass source parsing and remain
  readable.

A focused compatibility fixture deserializes a historical compiled policy
containing the `title_matches` discriminator. For each source fixture rewritten
from aliases or bare values, retain its pre-#350 compiled JSON as historical
evidence. Compile the canonical source, assert its `source_hash` matches the new
source bytes, then normalize only `source_hash` in the historical and current
values and require complete JSON equality. The active compiled golden updates
only its expected `source_hash`; every other field and operation remains
byte-for-byte equal.

## Canonical source migration

Current source artifacts that are compiled by tests or presented to operators
must use published syntax:

- replace `lang` with `language`;
- quote language and codec list values; and
- quote language equality values.

This includes `voom-policy` fixtures, cross-crate integration-test policy
strings, operator fixture policies, and the current operator runbook.
Historical specs, ADR discussion, audits, and parser-only tests are records of
past behavior and are not rewritten unless they are compiled as current source.

The published grammar corpus already uses canonical syntax. Its
`UNPUBLISHED_FORMS` guard is extended with representative bare-value spellings
so canonical corpus files cannot regress while compiler-focused negative tests
cover the complete rejection set.

## Failure behavior

Invalid filter source fails during policy validation before `compile_ast`.
There is no partial compiled policy and no persistence. Every consumer emits an
error diagnostic for the same invalid filter.

Malformed lists and Boolean expressions fail closed. The recognizer never
recovers by discarding empty elements, choosing the first valid prefix, or
ignoring trailing input. Numeric overflow and non-ASCII digits fail validation.

## Security and observability

Policy source is untrusted text. Complete consumption prevents an accepted
prefix from hiding a trailing operation-like token or parser-only spelling.
Quoted-string scanning honors escapes and never executes content.

No logging or telemetry change is required. Existing compile diagnostics expose
the source span and stable diagnostic code.

## Testing strategy

Behavior tests cover:

- every published leaf and its exact decoded compiled value;
- all six channel comparators and `u64` boundaries;
- nested grouping and `not`/`and`/`or` precedence;
- all seven filter consumers;
- aliases, bare equality/list values, `title matches`, unpublished comparators,
  malformed/empty lists, missing operands, unbalanced/empty groups, overflow,
  non-ASCII digits, and trailing input;
- lowering of every accepted leaf to the existing compiled representation;
- unchanged readable historical `title_matches` compiled JSON;
- historical/current compiled fixture equality after normalizing only
  `source_hash`; and
- the cross-crate published grammar corpus.

Focused verification:

```text
cargo test -p voom-policy track_filter
cargo test -p voom-policy policy_fixtures
cargo test -p voom-plan fixtures
cargo test -p voom-cli --test multi_phase_preview_envelope
cargo test -p voom-control-plane --test published_grammar_corpus
cargo test -p voom-control-plane --test remux_flow
cargo test -p voom-control-plane --test audio_transcode_flow
cargo test -p voom-control-plane --test audio_extract_flow
```

The full repository guardrail remains `just ci`.

## Rollback

The shared source parser, validation call sites, and lowering call sites are one
atomic rollback unit. Never restore acceptance of an alias without restoring
its exact lowering semantics and accepted-implies-lowerable regression test.
No stored data or schema rollback is needed. Policies compiled before the
change remain readable throughout.
