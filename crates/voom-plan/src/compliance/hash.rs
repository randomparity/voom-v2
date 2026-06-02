use serde_json::Value;

pub fn report_hash(report: &crate::ComplianceReport) -> Result<String, serde_json::Error> {
    let value = serde_json::to_value(report)?;
    report_hash_from_value(&value)
}

pub fn report_hash_from_value(value: &Value) -> Result<String, serde_json::Error> {
    let mut value = value.clone();
    strip_report_hash(&mut value);
    Ok(format!(
        "blake3:{}",
        blake3::hash(crate::hash::canonical_json(&value)?.as_bytes()).to_hex()
    ))
}

pub fn report_id(preimage: &Value) -> Result<String, serde_json::Error> {
    let hash = blake3::hash(crate::hash::canonical_json(preimage)?.as_bytes())
        .to_hex()
        .to_string();
    Ok(format!("report_{}", &hash[..16]))
}

#[must_use]
pub fn check_id(report_id_preimage: &str, node_id: &str, operation_kind: &str) -> String {
    let hash =
        blake3::hash(format!("{report_id_preimage}\n{node_id}\n{operation_kind}").as_bytes())
            .to_hex()
            .to_string();
    format!("check_{}", &hash[..16])
}

fn strip_report_hash(value: &mut Value) {
    if let Value::Object(map) = value {
        map.remove("report_hash");
    }
}

#[cfg(test)]
#[path = "hash_test.rs"]
mod tests;
