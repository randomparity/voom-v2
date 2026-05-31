//! Future home for reusable artifact domain types.
//!
//! This crate is intentionally empty today. Runtime artifact orchestration
//! lives in `voom-control-plane::artifact`, where it can coordinate store
//! transactions, worker dispatch, filesystem promotion, verification, and
//! control-plane events in one application boundary.
//!
//! Move code here only when a type or rule is independent of that orchestration
//! boundary and has at least one non-control-plane consumer.

pub mod placeholder {
    //! Empty marker module so the reserved crate remains a valid package.
}
