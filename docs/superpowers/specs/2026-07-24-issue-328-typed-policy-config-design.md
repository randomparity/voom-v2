# Issue #328: Typed policy config execution defaults

Date: 2026-07-24

## Goal

Lower the exact published `config` syntax into typed compiled policy data,
apply policy-level error strategy defaults before planning, and keep every
existing compiled policy version readable.

## Success criteria

- `languages: ["eng", "und"]` compiles to an ordered `Vec<String>`.
- `on_error: abort|continue` compiles to `Option<ErrorStrategy>`.
- A phase override wins over the policy value.
- A phase without an override receives the policy value before planning.
- Old raw-string compiled config JSON deserializes and receives the same
  effective defaults.
- Missing config fields deserialize to empty/implicit-abort defaults.
- Typed and legacy stored language values obey the same lowercase
  three-letter invariant.
- Explicit `where` selectors are byte-for-byte equivalent in compiled
  operations regardless of configured languages.
- Unpublished config source forms are rejected, not normalized as aliases.
- Whitespace-separated language lists are not newly accepted by the typed
  parser.
- `defaults ... best` remains visibly unsupported until #336.

## Existing flow

```text
source config statements
  -> StatementAst raw text
  -> BTreeMap<String, JsonValue::String(full_statement)>
  -> stored compiled_json
  -> no execution consumer
```

Phase-level `on_error` already lowers to `CompiledPhase.on_error`. The
phase-barrier coordinator rejects `continue` and `skip` before opening a job,
but it only sees explicit phase values. `defaults ... best` is separately
rejected by remux planning and selection.

## Proposed flow

```text
source config key/value settings
  -> SettingAst + ExprAst
  -> CompiledConfig { languages, on_error }
  -> stored typed compiled_json
  -> apply_execution_defaults()
  -> effective CompiledPhase.on_error
```

For an existing stored version:

```text
legacy raw-string config JSON
  -> CompiledConfig compatibility deserializer
  -> typed CompiledConfig
  -> apply_execution_defaults()
```

## Contract

### Source

Only these productions are accepted:

```text
languages: ["eng", ...]
on_error: abort|continue
```

Language entries must be quoted lowercase three-letter ASCII codes, with commas
between entries. Order is preserved. The compiler does not add
`languages audio`, bare language tokens, whitespace-separated list values, or
`skip` as config aliases.

### Compiled JSON

New shape:

```json
{
  "config": {
    "languages": ["eng", "und"],
    "on_error": "continue"
  }
}
```

Missing keys default to `[]` and `None`. Empty values are omitted when writing,
so a policy without config retains `"config": {}`.

The compatibility reader accepts the existing shapes:

```json
{
  "config": {
    "languages": "languages: [\"eng\", \"und\"]",
    "on_error": "on_error: continue"
  }
}
```

and the shape present in checked-in pre-change policy versions:

```json
{
  "config": {
    "languages": "languages audio: [eng, und]",
    "on_error": "on_error continue"
  }
}
```

The legacy `on_error` reader also accepts `skip` because the former compiler
accepted and lowered it. The compatibility reader recognizes the former
`languages audio|subtitle` targets and whitespace around or instead of the
colon because those shapes could be persisted as raw statements. New source
does not accept any of those aliases.

Compatibility parsing is intentionally restricted to these stored fields and
known legacy structures. Unknown config fields fail instead of being silently
discarded. Typed arrays and legacy statements both validate every
decoded language as a lowercase three-letter ASCII code. Invalid types,
non-string list entries, invalid language values, unknown legacy prefixes, or
unknown error-strategy values fail with the config field in the message.

An immutable `legacy-policy-config-v2.json` fixture retains the pre-change raw
wire representation. Golden regeneration never rewrites it. A paired source
test rejects `languages audio` so stored compatibility cannot leak into source
grammar.

### Effective phase strategy

For every phase:

```text
effective_on_error =
  phase.on_error
  ?? policy.config.on_error
  ?? abort
```

`apply_execution_defaults()` materializes only the configured policy value.
It leaves both fields absent when the implicit fallback is abort, preserving
the distinction between explicit and implicit abort where it is still useful.

The compiler calls this after lowering. Both stored-policy production loaders
call it after identity validation and before planning or coordinator preflight.

### Language preferences

The ordered vector is available on `CompiledPolicy.config.languages`. Nothing
in #328 ranks streams. No operation filter is rewritten. In particular:

```text
config { languages: ["eng"] }
defaults audio where language == "spa"
```

continues to lower the filter as `LanguageIn { values: ["spa"] }`.

## Error handling

- Source-shape errors are validation diagnostics with the offending setting
  span.
- Duplicate keys fail validation rather than silently taking first or last.
- Malformed stored JSON fails the existing plan-generation boundary before any
  execution mutation.
- Applying defaults is idempotent: a second call observes already-populated
  phase values and changes nothing.

## Tests

### `voom-policy`

- parser produces typed config settings and rejects comma-less language lists;
- canonical config lowers to typed fields in source order;
- duplicate/unknown/wrong-shape/unquoted language values fail;
- config `skip` fails while phase compatibility remains unchanged;
- new compiled JSON uses typed values;
- the immutable old raw-string fixture and missing fields deserialize;
- prior language targets and config whitespace deserialize only at the stored
  JSON boundary;
- unknown durable config fields fail loudly;
- malformed typed and legacy language values fail with context;
- policy defaults fill omitted phase values;
- explicit phase values win;
- explicit track filters are unchanged.

### `voom-control-plane`

- stored legacy config is normalized before accepted-policy planning;
- the execute loader receives legacy raw config with null phase values and
  policy-level `continue` reaches the pre-job fail-loud guard;
- the same legacy execute fixture proves phase-level `abort` overrides policy
  `continue`;
- no job is opened on an unsupported effective `continue`.

### Corpus and fixtures

- preserve the immutable pre-change compiled compatibility fixture;
- regenerate only current compiled goldens from canonical sources;
- keep the published grammar corpus green;
- update legacy sample source spellings to the published config syntax.

## Scope boundary

This change does not:

- select a stream for `defaults ... best` (#336);
- implement `on_error: continue` execution (#335);
- change condition evaluation (#329);
- change `run_if` evaluation (#330);
- add a DSL alias or unpublished production.

## Rollback

Revert the parser AST, typed compiled config, production normalization calls,
fixture updates, and tests together. No database migration is involved because
`compiled_json` remains a JSON value and the compatibility reader handles
pre-change rows.
