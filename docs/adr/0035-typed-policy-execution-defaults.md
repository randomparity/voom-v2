---
status: accepted
date: 2026-07-24
deciders: [VOOM core]
---

# 0035 — Published policy config lowers to typed execution defaults

## Context

The published V1 grammar defines two policy-level config settings:

```text
config {
  languages: ["eng", ...]
  on_error: abort|continue
}
```

The compiler currently stores each setting as its complete source line in
`CompiledPolicy.config`, a `BTreeMap<String, serde_json::Value>`. No execution
path consumes those strings. The language order is therefore unavailable to
the future `defaults audio|subtitle best` implementation, and policy-level
`on_error` does not default phases that omit an override.

Existing policy versions already persist the raw map in
`policy_versions.compiled_json`. Those versions must remain readable. At the
same time, newly compiled policies must not gain alternate source spellings
such as `languages audio: [...]`, unquoted language values, or `on_error: skip`;
none is part of the published grammar.

The detailed design is recorded in
[`docs/superpowers/specs/2026-07-24-issue-328-typed-policy-config-design.md`](../superpowers/specs/2026-07-24-issue-328-typed-policy-config-design.md).

## Decision

1. `PolicyAst.config` is a list of typed `key: value` settings, using the same
   expression parser as metadata. Validation accepts exactly:

   - `languages`: a list of quoted, lowercase three-letter ASCII language
     codes;
   - `on_error`: the identifier `abort` or `continue`.

   Repeated config keys fail validation. Unknown keys retain the existing
   unknown-config diagnostic behavior. The parser does not add aliases or
   unpublished productions.

2. `CompiledPolicy.config` becomes `CompiledConfig`, with:

   - `languages: Vec<String>` preserving source order;
   - `on_error: Option<ErrorStrategy>`.

   Empty fields use serde defaults and are omitted when serialized. Newly
   compiled JSON therefore contains typed arrays and enum strings rather than
   statement source.

3. `CompiledConfig` has a narrow compatibility deserializer for existing
   compiled policy JSON. It accepts the previous raw statement strings for the
   two known keys and normalizes them into the typed fields. Serialization
   emits only the typed shape. Malformed legacy values fail deserialization
   with field context.

   This compatibility is for stored compiled JSON only. It does not make legacy
   source forms valid in the parser or validator.

4. After lowering, and after every production deserialization of a stored
   compiled policy, `CompiledPolicy::apply_execution_defaults()` copies
   `config.on_error` into phases whose `on_error` is absent. Explicit phase
   values are unchanged. With no configured value, absence retains the
   existing implicit-abort behavior.

5. Language preferences remain policy data in this change. They do not rewrite
   any `TrackFilter`, filter-addressed default, or other explicit selector.
   Issue #336 will use the ordered list to resolve `defaults ... best` to a
   concrete stream. This change does not make `best` executable.

## Consequences

- New compiled policies expose typed execution defaults directly to
  `voom-plan` and the coordinator.
- Policy-level `on_error: continue` reaches the existing fail-loud coordinator
  guard even when a phase omits an override. Issue #335 will replace that guard
  with continue semantics.
- Phase overrides remain authoritative.
- Existing compiled policy versions deserialize and acquire the same effective
  defaults before planning.
- Canonical compiled-policy fixtures change only in their config representation
  and effective phase `on_error` values.
- Existing non-published sample policies using `languages audio: [...]` are
  rewritten to the published syntax; no new source syntax is introduced.

## Considered and rejected

### Keep the raw map and add typed accessors

Rejected because new compiled output would continue storing source text as
execution state. Every consumer would also need to handle malformed map values
and repeat default resolution.

### Add a second typed field beside the raw map

Rejected because duplicated durable state can disagree. A single typed config
field, with read compatibility at its serde boundary, gives one source of truth.

### Implement `defaults ... best` here

Rejected as #336 scope. This issue supplies its ordered preference input but
does not choose or carry a stream identity.

### Rewrite explicit language selectors from config

Rejected because policy defaults must not change explicit `where` intent.
