use voom_core::VoomError;

#[derive(Debug)]
pub(crate) enum OrderedTicketResult {
    Outputs(Vec<serde_json::Value>),
    Scalar(serde_json::Value),
}

pub(crate) fn ordered_ticket_result(result: &str) -> Result<OrderedTicketResult, VoomError> {
    let value: serde_json::Value = serde_json::from_str(result)
        .map_err(|error| VoomError::database(format!("ticket result is malformed: {error}")))?;
    let outputs = value
        .get("outputs")
        .and_then(serde_json::Value::as_array)
        .filter(|outputs| !outputs.is_empty());
    match outputs {
        Some(outputs) => Ok(OrderedTicketResult::Outputs(outputs.clone())),
        None => Ok(OrderedTicketResult::Scalar(value)),
    }
}

pub(crate) fn result_location_ids(result: &str) -> Result<Vec<u64>, VoomError> {
    let members = match ordered_ticket_result(result)? {
        OrderedTicketResult::Outputs(outputs) => outputs,
        OrderedTicketResult::Scalar(value) => vec![value],
    };
    let mut ids = Vec::new();
    for member in members {
        let Some(value) = member.get("result_file_location_id") else {
            continue;
        };
        if let Some(id) = value.as_u64() {
            ids.push(id);
        } else if value.as_i64().is_some() {
            return Err(VoomError::database(format!(
                "promotion ticket result location id is invalid: {value}"
            )));
        }
    }
    Ok(ids)
}

#[cfg(test)]
#[path = "ticket_results_test.rs"]
mod tests;
