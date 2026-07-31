-- Canonicalize remote acquire replay responses written before scheduler decisions.

UPDATE remote_idempotency_keys
SET response_json = json_set(response_json, '$.data.scheduler_decision_id', 0)
WHERE route_key = 'POST /v1/execution/lease/acquire'
  AND status = 'completed'
  AND json_extract(response_json, '$.status') = 'ok'
  AND json_type(response_json, '$.data') = 'object'
  AND json_extract(response_json, '$.data.outcome') IN ('idle', 'no_candidate', 'leased')
  AND json_type(response_json, '$.data.scheduler_decision_id') IS NULL;
