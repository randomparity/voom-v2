-- Issue #400: typed NVIDIA video profiles and exclusive local device claims.

CREATE TABLE video_profiles_new (
  id               TEXT PRIMARY KEY,
  name             TEXT NOT NULL UNIQUE,
  target_codec     TEXT NOT NULL,
  encoder          TEXT NOT NULL,
  crf              INTEGER,
  cq               INTEGER,
  preset           TEXT NOT NULL,
  tune             TEXT,
  codec_profile    TEXT,
  codec_level      TEXT,
  pixel_format     TEXT,
  max_width        INTEGER,
  max_height       INTEGER,
  output_container TEXT NOT NULL DEFAULT 'mkv',
  copy_compatible  INTEGER NOT NULL DEFAULT 0,
  retired_at       TEXT,
  decode_backend   TEXT NOT NULL DEFAULT 'software',
  CHECK (length(trim(name)) > 0),
  CHECK (target_codec IN ('hevc', 'av1')),
  CHECK (encoder IN ('libx265', 'libsvtav1', 'libaom-av1', 'hevc_nvenc')),
  CHECK (
    (encoder = 'hevc_nvenc' AND crf IS NULL AND cq BETWEEN 1 AND 51)
    OR
    (encoder != 'hevc_nvenc' AND crf >= 0 AND cq IS NULL)
  ),
  CHECK (max_width IS NULL OR max_width > 0),
  CHECK (max_height IS NULL OR max_height > 0),
  CHECK (output_container IN ('mkv', 'mp4')),
  CHECK (copy_compatible IN (0, 1)),
  CHECK (decode_backend IN ('software', 'nvidia')),
  CHECK (decode_backend = 'software' OR encoder = 'hevc_nvenc')
) STRICT;

INSERT INTO video_profiles_new (
  id,
  name,
  target_codec,
  encoder,
  crf,
  cq,
  preset,
  tune,
  codec_profile,
  codec_level,
  pixel_format,
  max_width,
  max_height,
  output_container,
  copy_compatible,
  retired_at,
  decode_backend
)
SELECT
  id,
  name,
  target_codec,
  encoder,
  crf,
  NULL,
  preset,
  tune,
  codec_profile,
  codec_level,
  pixel_format,
  max_width,
  max_height,
  output_container,
  copy_compatible,
  retired_at,
  'software'
FROM video_profiles;

DROP TABLE video_profiles;
ALTER TABLE video_profiles_new RENAME TO video_profiles;

CREATE TABLE accelerator_claims (
  hardware_token         TEXT PRIMARY KEY,
  backend                TEXT NOT NULL,
  worker_id              INTEGER NOT NULL UNIQUE REFERENCES workers(id) ON DELETE RESTRICT,
  boot_id                TEXT NOT NULL,
  supervisor_pid         INTEGER NOT NULL,
  supervisor_start_ticks INTEGER NOT NULL,
  process_group_id       INTEGER NOT NULL,
  capacity               INTEGER NOT NULL,
  claimed_at             TEXT NOT NULL,
  CHECK (length(trim(hardware_token)) > 0),
  CHECK (backend = 'nvidia'),
  CHECK (supervisor_pid > 0),
  CHECK (supervisor_start_ticks > 0),
  CHECK (process_group_id > 0),
  CHECK (capacity BETWEEN 1 AND 16)
) STRICT;
