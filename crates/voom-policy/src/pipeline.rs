use crate::{
    DiagnosticSeverity, PolicyAst, PolicyDiagnostic, ValidationResult, compile_ast,
    parse_policy_source, validate_policy_ast,
};

#[derive(Debug, Clone, PartialEq)]
pub struct CompileOutput {
    pub policy: crate::CompiledPolicy,
    pub diagnostics: Vec<PolicyDiagnostic>,
}

#[derive(Debug)]
pub struct PolicyCompileError {
    pub error: voom_core::VoomError,
    pub diagnostics: Vec<PolicyDiagnostic>,
}

impl PolicyCompileError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.error.code()
    }
}

pub fn parse_policy(source: &str) -> Result<PolicyAst, PolicyCompileError> {
    parse_policy_source(source).map_err(|err| PolicyCompileError {
        error: voom_core::VoomError::PolicyParseError("policy parse failed".to_owned()),
        diagnostics: err.diagnostics,
    })
}

pub fn validate_policy(source: &str) -> Result<ValidationResult, PolicyCompileError> {
    let ast = parse_policy(source)?;
    let result = validate_policy_ast(source, &ast);
    if has_errors(&result.diagnostics) {
        Err(PolicyCompileError {
            error: voom_core::VoomError::PolicyValidationError(
                "policy validation failed".to_owned(),
            ),
            diagnostics: result.diagnostics,
        })
    } else {
        Ok(result)
    }
}

pub fn compile_policy(source: &str) -> Result<CompileOutput, PolicyCompileError> {
    let ast = parse_policy(source)?;
    let validation = validate_policy_ast(source, &ast);
    if has_errors(&validation.diagnostics) {
        return Err(PolicyCompileError {
            error: voom_core::VoomError::PolicyValidationError(
                "policy validation failed".to_owned(),
            ),
            diagnostics: validation.diagnostics,
        });
    }

    let warnings = validation
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Warning)
        .cloned()
        .collect::<Vec<_>>();
    let policy = compile_ast(source, &ast, warnings).map_err(|diagnostics| PolicyCompileError {
        error: voom_core::VoomError::PolicyValidationError("policy compile failed".to_owned()),
        diagnostics,
    })?;
    Ok(CompileOutput {
        policy,
        diagnostics: validation.diagnostics,
    })
}

fn has_errors(diagnostics: &[PolicyDiagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
}

#[cfg(test)]
#[path = "pipeline_test.rs"]
mod tests;
