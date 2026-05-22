pub mod binding;
pub mod model;
pub mod ticket_payload;
pub mod timing;

pub use model::{
    ConcurrencyPolicy, FanOutPolicy, OperationNode, TimingPolicy, WorkflowNode, WorkflowPlan,
};

#[cfg(test)]
#[path = "model_test.rs"]
mod model_tests;

#[cfg(test)]
#[path = "ticket_payload_test.rs"]
mod ticket_payload_tests;

#[cfg(test)]
#[path = "binding_test.rs"]
mod binding_tests;

#[cfg(test)]
#[path = "timing_test.rs"]
mod timing_tests;
