-- Issue #409: typed VAAPI video profiles.
--
-- Table rebuild rather than ALTER: `preset` drops NOT NULL (`hevc_vaapi` has no
-- `-preset`) and the quality CHECK has to name three mutually exclusive columns,
-- neither of which SQLite can express with ALTER TABLE.

CREATE TABLE video_profiles_new (
  id               TEXT PRIMARY KEY,
  name             TEXT NOT NULL UNIQUE,
  target_codec     TEXT NOT NULL,
  encoder          TEXT NOT NULL,
  crf              INTEGER,
  cq               INTEGER,
  qp               INTEGER,
  preset           TEXT,
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
  CHECK (encoder IN ('libx265', 'libsvtav1', 'libaom-av1', 'hevc_nvenc', 'hevc_vaapi')),
  -- Exactly one quality field per encoder, the one its QualityDomain names in
  -- voom-core. `qp` excludes 0 because hevc_vaapi reads 0 as "auto", which is
  -- not an operator quality target.
  --
  -- Each arm asserts its own field IS NOT NULL before the range test: SQLite
  -- treats a CHECK that evaluates to NULL as satisfied, so a bare
  -- `cq BETWEEN 1 AND 51` admits a row with no quality field at all.
  CHECK (
    (encoder = 'hevc_nvenc'
      AND crf IS NULL AND qp IS NULL
      AND cq IS NOT NULL AND cq BETWEEN 1 AND 51)
    OR
    (encoder = 'hevc_vaapi'
      AND crf IS NULL AND cq IS NULL
      AND qp IS NOT NULL AND qp BETWEEN 1 AND 52)
    OR
    (encoder NOT IN ('hevc_nvenc', 'hevc_vaapi')
      AND cq IS NULL AND qp IS NULL
      AND crf IS NOT NULL AND crf >= 0)
  ),
  CHECK (
    (encoder = 'hevc_vaapi' AND preset IS NULL)
    OR
    (encoder != 'hevc_vaapi' AND preset IS NOT NULL)
  ),
  CHECK (max_width IS NULL OR max_width > 0),
  CHECK (max_height IS NULL OR max_height > 0),
  CHECK (output_container IN ('mkv', 'mp4')),
  CHECK (copy_compatible IN (0, 1)),
  CHECK (decode_backend IN ('software', 'nvidia', 'vaapi')),
  CHECK (
    decode_backend = 'software'
    OR (decode_backend = 'nvidia' AND encoder = 'hevc_nvenc')
    OR (decode_backend = 'vaapi' AND encoder = 'hevc_vaapi')
  )
) STRICT;

INSERT INTO video_profiles_new (
  id,
  name,
  target_codec,
  encoder,
  crf,
  cq,
  qp,
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
  cq,
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
  decode_backend
FROM video_profiles;

DROP TABLE video_profiles;
ALTER TABLE video_profiles_new RENAME TO video_profiles;
