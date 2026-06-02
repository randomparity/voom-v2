mod conditions;
mod operations;
mod phases;

use crate::{PolicyAst, PolicyDiagnostic};

use super::compiled::{CompiledPolicy, PolicyProvenance, slug, source_hash};

pub(crate) fn compile_ast(
    source: &str,
    ast: &PolicyAst,
    warnings: Vec<PolicyDiagnostic>,
) -> Result<CompiledPolicy, Vec<PolicyDiagnostic>> {
    let compiled_phases = phases::lower_phases(source, ast)?;
    Ok(CompiledPolicy {
        policy_name: ast.name.value.clone(),
        slug: slug(&ast.name.value),
        source_hash: source_hash(source),
        schema_version: 2,
        metadata: phases::metadata_map(&ast.metadata),
        config: phases::config_map(&ast.config),
        phase_order: phases::phase_order(ast),
        phases: compiled_phases,
        warnings,
        provenance: PolicyProvenance::default(),
    })
}
