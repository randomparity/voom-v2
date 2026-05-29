CREATE TABLE video_profiles (
  id               TEXT PRIMARY KEY,
  name             TEXT NOT NULL UNIQUE,
  target_codec     TEXT NOT NULL,
  encoder          TEXT NOT NULL,
  crf              INTEGER NOT NULL,
  preset           TEXT NOT NULL,
  tune             TEXT,
  codec_profile    TEXT,
  codec_level      TEXT,
  pixel_format     TEXT,
  max_width        INTEGER,
  max_height       INTEGER,
  output_container TEXT NOT NULL DEFAULT 'mkv',
  copy_compatible  INTEGER NOT NULL DEFAULT 0,
  CHECK (length(trim(name)) > 0),
  CHECK (target_codec IN ('hevc', 'av1')),
  CHECK (encoder IN ('libx265', 'libsvtav1', 'libaom-av1')),
  CHECK (crf >= 0),
  CHECK (max_width IS NULL OR max_width > 0),
  CHECK (max_height IS NULL OR max_height > 0),
  CHECK (output_container IN ('mkv', 'mp4')),
  CHECK (copy_compatible IN (0, 1))
) STRICT;

INSERT INTO video_profiles
  (id, name, target_codec, encoder, crf, preset, codec_profile, pixel_format,
   max_width, max_height, output_container, copy_compatible)
VALUES
  ('vp-default-hevc', 'default-hevc', 'hevc', 'libx265', 23, 'medium',
   NULL, NULL, NULL, NULL, 'mkv', 0),
  ('vp-hevc-archive', 'hevc-archive', 'hevc', 'libx265', 18, 'slow',
   'main10', 'yuv420p10le', NULL, NULL, 'mkv', 0),
  ('vp-hevc-1080p', 'hevc-1080p', 'hevc', 'libx265', 23, 'medium',
   NULL, NULL, 1920, 1080, 'mp4', 1),
  ('vp-default-av1', 'default-av1', 'av1', 'libsvtav1', 30, '8',
   NULL, NULL, NULL, NULL, 'mkv', 0),
  ('vp-av1-archive', 'av1-archive', 'av1', 'libaom-av1', 20, '4',
   NULL, NULL, NULL, NULL, 'mkv', 0),
  ('vp-av1-1080p', 'av1-1080p', 'av1', 'libsvtav1', 32, '8',
   NULL, NULL, 1920, 1080, 'mp4', 1);
