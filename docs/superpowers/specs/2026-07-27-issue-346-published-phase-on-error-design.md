# Issue #346: Published phase `on_error` grammar

Date: 2026-07-27
Base: `main` at `4fc6bf74e00dba51b253d084c7cb7654cf3df47c`

## Goal

Accept only the published phase `on_error: abort|continue` production for new
policy source while preserving every previously persisted compiled-policy
representation of `ErrorStrategy::Skip`.

## Governing decision

Accepted ADR 0035 defines `abort|continue` as the source grammar and explicitly
keeps `skip` only in the stored compiled-policy compatibility reader. This
change applies that existing decision to phase controls; it does not introduce
a new architectural decision.

## Existing behavior

Phase controls remain raw `StatementAst` text. Validation obtains their value
through `setting_value`, which deliberately accepts either colon-delimited
settings or a second whitespace-delimited word. It also recognizes `skip`.
Lowering maps all three values into `ErrorStrategy`.

The durable representation separately derives serde support for
`ErrorStrategy`, and `CompiledConfig` has a compatibility decoder for legacy
raw `on_error` strings. Those read paths must remain unchanged.

## Source contract

The accepted production requires a colon:

```text
on_error: abort|continue
```

The colon is mandatory. A phase-specific validator reads the original source
through the bounds- and character-boundary-checked
`Validator::source.get(statement.span().start..statement.span().end)` rather
than the Unicode-trimmed `StatementAst::Raw.text` or compatibility-oriented
`setting_value` helper. It checks that complete source slice:

1. the statement starts with the parser-recognized exact keyword `on_error`;
2. only ASCII policy whitespace may occur between that keyword and one
   mandatory colon;
3. only ASCII policy whitespace may surround the value after the colon; and
4. the remaining value is exactly the identifier `abort` or `continue`.

Using the original span is required because the parser constructs raw statement
text with Unicode-aware `str::trim`, which would otherwise hide terminal
non-ASCII whitespace before a newline or closing brace. The source-slice check
matches the typed setting parser's ASCII-whitespace behavior.
`on_error:abort` and `on_error : continue` are therefore grammatical formatting
variants rather than aliases. Junk or a value before the colon, a second colon,
Unicode whitespace, quoted/case variants, trailing tokens, an absent value,
`skip`, and every colonless form fail with `InvalidOnErrorValue`.

Validation owns source acceptance. Lowering retains mappings only for
`abort|continue`, ensuring no source-only path can emit a fresh compiled
`Skip`.

## Compatibility boundary

`ErrorStrategy::Skip` remains in the public compiled type and retains its
serde `"skip"` wire value. The legacy `CompiledConfig` string decoder also
continues to recognize `on_error skip`. An immutable whole-`CompiledPolicy`
JSON fixture with a phase containing `"on_error": "skip"` will deserialize to
the typed enum and reserialize to the identical JSON value. The existing config
compatibility test continues to cover the legacy raw string. Compiling each
published phase value will also assert its exact serialized `"abort"` or
`"continue"` field.

New source can never reach either compatibility path. No compiled field,
variant, serialization shape, or policy source hash behavior changes.

## Failure behavior

Rejected source returns the existing compile validation error with an
`invalid_on_error_value` diagnostic attached to the complete phase statement.
If a caller passes `validate_policy_ast` an AST whose span is out of bounds or
not on UTF-8 character boundaries for the supplied source, checked extraction
fails into that same diagnostic rather than panicking. Compilation does not
lower or emit a compiled policy after validation errors. There is no durable
mutation in `voom-policy`.

## Dependencies and exclusions

- Issue #344 strengthens the broader durable payload guard. This change does
  not depend on that implementation because it preserves the current enum
  shape and verifies decoding directly.
- Issue #335 owns runtime execution of the published `continue` strategy.
  This change does not alter coordinator semantics.
- Issue #338 owns full published-grammar corpus execution.
- Config syntax and config compatibility parsing are unchanged.
- Parser restructuring is excluded: the validator already owns phase-control
  grammar checks, and converting all phase controls to typed AST settings would
  be disproportionate.

## Security and rollback

The input surface only narrows, so no new parser capability is exposed.
Malformed or hostile values fail before lowering. Rollback remains compatible:
old binaries and new binaries share the same compiled enum wire shape, and new
binaries continue reading historical `"skip"` values.

## Verification

- Compile policies containing phase `on_error: abort` and
  `on_error: continue`, asserting the resulting enum values and exact serialized
  field strings.
- Reject colonless `abort`, colonless `continue`, `skip` with and without a
  colon, missing and trailing values, junk/value text before the colon, double
  colons, quoted/case variants, and non-ASCII whitespace between the keyword
  and colon, after the colon, and after each valid value immediately before
  both a newline and `}`. Assert the exact `invalid_on_error_value` diagnostic
  for each.
- Parse an `on_error` AST from one source, validate it against a shorter or
  differently aligned source, and assert an `invalid_on_error_value` diagnostic
  without a panic.
- Deserialize an immutable whole-policy fixture whose phase contains `"skip"`,
  assert `ErrorStrategy::Skip`, and reserialize to the identical JSON value.
- Retain the existing legacy config-string compatibility test.
- Re-run this focused matrix after rebasing onto #344, before merge.
- Run focused `voom-policy` tests, formatting, linting, and `just ci`.
