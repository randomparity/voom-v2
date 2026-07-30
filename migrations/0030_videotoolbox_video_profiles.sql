-- Issue #411: typed VideoToolbox video profiles.

CREATE TABLE video_profiles_new (
  id               TEXT PRIMARY KEY,
  name             TEXT NOT NULL UNIQUE,
  target_codec     TEXT NOT NULL,
  encoder          TEXT NOT NULL,
  crf              INTEGER,
  cq               INTEGER,
  bitrate_kbps     INTEGER,
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
  CHECK (target_codec IN ('h264', 'hevc', 'av1')),
  CHECK (
    encoder IN (
      'libx265',
      'libsvtav1',
      'libaom-av1',
      'hevc_nvenc',
      'h264_videotoolbox',
      'hevc_videotoolbox'
    )
  ),
  CHECK (
    (
      encoder IN ('h264_videotoolbox', 'hevc_videotoolbox')
      AND crf IS NULL
      AND cq IS NULL
      AND bitrate_kbps BETWEEN 1 AND 4294967295
    )
    OR (
      encoder = 'hevc_nvenc'
      AND crf IS NULL
      AND cq BETWEEN 1 AND 51
      AND bitrate_kbps IS NULL
    )
    OR (
      encoder NOT IN ('hevc_nvenc', 'h264_videotoolbox', 'hevc_videotoolbox')
      AND crf >= 0
      AND cq IS NULL
      AND bitrate_kbps IS NULL
    )
  ),
  CHECK (max_width IS NULL OR max_width > 0),
  CHECK (max_height IS NULL OR max_height > 0),
  CHECK (output_container IN ('mkv', 'mp4')),
  CHECK (copy_compatible IN (0, 1)),
  CHECK (decode_backend IN ('software', 'nvidia', 'video_toolbox')),
  CHECK (
    decode_backend = 'software'
    OR (decode_backend = 'nvidia' AND encoder = 'hevc_nvenc')
    OR (
      decode_backend = 'video_toolbox'
      AND encoder IN ('h264_videotoolbox', 'hevc_videotoolbox')
    )
  )
) STRICT;

INSERT INTO video_profiles_new (
  id,
  name,
  target_codec,
  encoder,
  crf,
  cq,
  bitrate_kbps,
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
