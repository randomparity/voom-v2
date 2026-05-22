pub mod binding;
pub mod executor;
pub mod expansion;
pub mod model;
pub mod runtime;
pub mod ticket_payload;
pub mod timing;

pub use executor::{WorkflowChaosOptions, WorkflowExecutor, WorkflowRunError, WorkflowRunSummary};
pub use model::{
    ConcurrencyPolicy, FanOutPolicy, OperationNode, TimingPolicy, WorkflowNode, WorkflowPlan,
};
pub use runtime::WorkerRuntimeRegistry;

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

#[cfg(test)]
#[path = "expansion_test.rs"]
mod expansion_tests;

#[cfg(test)]
#[path = "runtime_test.rs"]
mod runtime_tests;

#[cfg(test)]
#[path = "executor_test.rs"]
mod executor_tests;
