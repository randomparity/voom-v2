-- Issue #411: backend-neutral accelerator descriptors and claims.

UPDATE worker_capabilities
SET extra = json_set(extra, '$.accelerator.backend', 'nvidia')
WHERE json_type(extra, '$.accelerator') = 'object'
  AND json_type(extra, '$.accelerator.backend') IS NULL;

CREATE TABLE accelerator_claims_new (
  hardware_token            TEXT PRIMARY KEY,
  backend                   TEXT NOT NULL,
  worker_id                 INTEGER NOT NULL UNIQUE REFERENCES workers(id) ON DELETE RESTRICT,
  boot_id                   TEXT NOT NULL,
  supervisor_pid            INTEGER NOT NULL,
  supervisor_start_identity TEXT,
  process_group_id          INTEGER NOT NULL,
  capacity                  INTEGER NOT NULL,
  claimed_at                TEXT NOT NULL,
  CHECK (length(trim(hardware_token)) > 0),
  CHECK (backend IN ('nvidia', 'video_toolbox')),
  CHECK (supervisor_pid > 0),
  CHECK (
    (backend = 'nvidia' AND supervisor_start_identity IS NOT NULL)
    OR (backend = 'video_toolbox' AND supervisor_start_identity IS NULL)
  ),
  CHECK (process_group_id > 0),
  CHECK (capacity BETWEEN 1 AND 16)
) STRICT;

INSERT INTO accelerator_claims_new (
  hardware_token,
  backend,
  worker_id,
  boot_id,
  supervisor_pid,
  supervisor_start_identity,
  process_group_id,
  capacity,
  claimed_at
)
SELECT
  hardware_token,
  backend,
  worker_id,
  boot_id,
  supervisor_pid,
  'linux-proc-ticks:' || supervisor_start_ticks,
  process_group_id,
  capacity,
  claimed_at
FROM accelerator_claims;

DROP TABLE accelerator_claims;
ALTER TABLE accelerator_claims_new RENAME TO accelerator_claims;
