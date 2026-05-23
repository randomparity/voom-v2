use crate::{ExecutionPlan, PlanningRequest};

pub type PlanGenerationError = voom_core::VoomError;

pub fn generate_plan(_request: PlanningRequest) -> Result<ExecutionPlan, PlanGenerationError> {
    Err(voom_core::VoomError::PlanGeneration(
        "plan generation is not implemented yet".to_owned(),
    ))
}
