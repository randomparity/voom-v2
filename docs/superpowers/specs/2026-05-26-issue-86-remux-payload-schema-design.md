# Issue 86 Remux Payload Schema Design

## Context

Issue #86 is still present. Remux operation payloads are built as raw JSON in
`voom-plan::planner`, manually validated as raw JSON in
`voom-control-plane::workflow::binding`, and parsed again into a private shape in
`voom-control-plane::remux::selection`. The contracts differ: binding requires
`track_actions`, `track_order`, and `defaults`, while execution supplies serde
defaults for the same fields.

## Goals

- Define one shared typed model for policy remux operation payloads.
- Keep the payload JSON shape stable for existing plan fixtures and workflow
  ticket payloads.
- Preserve execution-only validation in remux selection, especially checks that
  require source media facts.
- Remove duplicated raw JSON validation from workflow binding.

## Non-Goals

- Do not redesign remux planning, grouping, or selection semantics.
- Do not move worker request or result schemas.
- Do not change ticket envelope fields outside the nested `remux` operation
  payload.

## Design

Add public remux payload types to `voom-plan::remux`:

- `RemuxOperationPayload`
- `RemuxTrackAction`
- `RemuxTrackActionKind`
- `RemuxDefaultAction`

The shared type owns the operation-payload contract:

- `type` is serialized as `"remux"` and must deserialize as `"remux"`.
- `container` is required and must be `"mkv"`.
- `source_media_snapshot_id` is optional on the shared struct so the planner can
  keep rendering diagnostic/test payloads before a persisted snapshot ID exists.
  Binding and execution use a strict parser that requires it to be present and
  positive before a workflow ticket can execute.
- `track_actions` and `defaults` default to empty arrays during deserialization.
- `track_order` defaults to the canonical `video`, `audio`, `subtitle` order
  during deserialization.
- Serialization from the planner still emits those fields explicitly.
- track actions reject unsupported action kinds and attachment targets.
- track order rejects empty, duplicate, attachment, and unknown groups.

The planner will construct `RemuxOperationPayload` and serialize it with serde
instead of assembling JSON fields by hand. Workflow binding will call a strict
`RemuxOperationPayload::try_from_execution_value` parser and embed the validated
value back into the workflow payload. Remux selection will parse the same strict
type and keep checks that require snapshot facts, such as video presence,
attachment streams, and default strategy support.

## Error Handling

The shared parser returns a small error type with human-readable messages. The
control-plane maps those messages to `BindingError` or `VoomError::Config` at its
boundary. Exact wording may change, but errors remain explicit and tied to the
remux payload field being rejected.

## Testing

- Add `voom-plan::remux` unit tests for defaulted fields, invalid type,
  unsupported container, missing/zero source snapshot id, malformed actions, and
  invalid track order.
- Keep binding tests focused on source wrapping and mapping typed payload errors
  into binding errors.
- Keep selection tests focused on media-fact-dependent behavior.
- Run targeted plan/control-plane tests, then `just ci`.
