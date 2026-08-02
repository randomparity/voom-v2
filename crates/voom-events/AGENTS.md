# Events-specific agent guidance

This file supplements the repository-root `AGENTS.md` for `voom-events`.

## Durable event contracts

- Event payload fields that identify one durable entity use the existing domain ID newtype,
  including IDs inside vectors. Transparent serde preserves the numeric JSON representation;
  primitive storage is not required for wire compatibility.
- Preserve those newtypes at producer call sites and in intermediate event structures. A
  typed final payload does not help if producers unwrap several IDs to `u64` and rebuild them
  immediately before serialization.
- Durable payload structs deny unknown fields on the actual serde unit. Tagged enum variants
  remain newtype variants over those structs; do not use inline tagged struct variants.
- Any payload shape or type change updates the payload contract inventory and scope file in
  the same change. Tests cover JSON compatibility, unknown-field rejection, and distinct
  sentinel values for every same-shaped ID so mapping mistakes are observable.
