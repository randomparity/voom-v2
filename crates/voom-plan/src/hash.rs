use serde_json::Value;

use crate::ExecutionPlan;

pub fn plan_hash(plan: &ExecutionPlan) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(plan)?;
    strip_volatile_plan_fields(&mut value);
    Ok(format!(
        "blake3:{}",
        blake3::hash(canonical_json(&value)?.as_bytes()).to_hex()
    ))
}

pub fn plan_id(preimage: &Value) -> Result<String, serde_json::Error> {
    let hash = blake3::hash(canonical_json(preimage)?.as_bytes())
        .to_hex()
        .to_string();
    Ok(format!("plan_{}", &hash[..16]))
}

#[must_use]
pub fn node_id(phase_name: &str, ordinal: u32, operation_kind: &str, target_key: &str) -> String {
    stable_prefixed_id(
        "node",
        &format!("{phase_name}\n{ordinal}\n{operation_kind}\n{target_key}"),
    )
}

#[must_use]
pub fn edge_id(from_node_id: &str, to_node_id: &str, dependency_kind: &str) -> String {
    stable_prefixed_id(
        "edge",
        &format!("{from_node_id}\n{to_node_id}\n{dependency_kind}"),
    )
}

pub fn canonical_json(value: &Value) -> Result<String, serde_json::Error> {
    let mut value = value.clone();
    sort_object_keys(&mut value);
    serde_json::to_string(&value)
}

fn stable_prefixed_id(prefix: &str, preimage: &str) -> String {
    let hash = blake3::hash(preimage.as_bytes()).to_hex().to_string();
    format!("{prefix}_{}", &hash[..16])
}

fn strip_volatile_plan_fields(value: &mut Value) {
    if let Value::Object(map) = value {
        map.remove("plan_id");
        map.remove("plan_hash");
        map.remove("generated_at");
    }
}

fn sort_object_keys(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for value in map.values_mut() {
                sort_object_keys(value);
            }
            map.sort_keys();
        }
        Value::Array(values) => {
            for value in values {
                sort_object_keys(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[cfg(test)]
#[path = "hash_test.rs"]
mod tests;
