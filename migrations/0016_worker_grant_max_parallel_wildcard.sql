-- Normalize legacy worker grant max_parallel rows.
--
-- The runtime now reads operation-specific keys first, then "*". Older test
-- and seed data used {"limit": n}; pure legacy rows become {"*": n}, while
-- mixed rows keep their current operation/wildcard keys and drop "limit".

UPDATE worker_grants
SET max_parallel = json_object('*', json_extract(max_parallel, '$.limit'))
WHERE json_type(max_parallel, '$.limit') IS NOT NULL
  AND json_remove(max_parallel, '$.limit') = '{}';

UPDATE worker_grants
SET max_parallel = json_remove(max_parallel, '$.limit')
WHERE json_type(max_parallel, '$.limit') IS NOT NULL;
