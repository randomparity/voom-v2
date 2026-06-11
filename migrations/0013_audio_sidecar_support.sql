-- Sprint 14 -- audio sidecar commit persistence support.
--
-- SQLite CHECK constraints require table rebuilds. The same COMMIT / PRAGMA /
-- BEGIN pattern as 0012 is used here for the same reason: sqlx wraps each
-- migration in a transaction, but SQLite silently ignores session PRAGMAs
-- (foreign_keys, legacy_alter_table) when an open transaction is active.  We
-- COMMIT the migrator's transaction, apply the PRAGMAs while no transaction is
-- open so they take effect, then BEGIN a new transaction for sqlx's checksum
-- bookkeeping.
COMMIT;
PRAGMA foreign_keys = OFF;
PRAGMA legacy_alter_table = ON;

ALTER TABLE asset_bundle_members RENAME TO asset_bundle_members_old;

CREATE TABLE asset_bundle_members (
    id              INTEGER PRIMARY KEY,
    bundle_id       INTEGER NOT NULL REFERENCES asset_bundles(id) ON DELETE CASCADE,
    file_asset_id   INTEGER NOT NULL REFERENCES file_assets(id) ON DELETE RESTRICT,
    role            TEXT NOT NULL
        CHECK (role IN ('primary_video','commentary_audio','external_audio',
                        'external_subtitle','poster','nfo','trailer','transcript',
                        'thumbnail','report')),
    UNIQUE (file_asset_id)
) STRICT;

INSERT INTO asset_bundle_members (id, bundle_id, file_asset_id, role)
SELECT id, bundle_id, file_asset_id, role
FROM asset_bundle_members_old;

DROP TABLE asset_bundle_members_old;

CREATE INDEX asset_bundle_members_by_bundle ON asset_bundle_members (bundle_id);

ALTER TABLE file_versions RENAME TO file_versions_old;

CREATE TABLE file_versions (
    id                          INTEGER PRIMARY KEY,
    file_asset_id               INTEGER NOT NULL REFERENCES file_assets(id) ON DELETE RESTRICT,
    content_hash                TEXT NOT NULL,
    size_bytes                  INTEGER NOT NULL CHECK (size_bytes >= 0),
    produced_by                 TEXT NOT NULL
        CHECK (produced_by IN ('ingest','transcode','remux','restore','external_observed','staged_commit')),
    produced_from_version_id    INTEGER REFERENCES file_versions(id) ON DELETE RESTRICT,
    created_at                  TEXT NOT NULL,
    retired_at                  TEXT,
    epoch                       INTEGER NOT NULL DEFAULT 0,
    CHECK (
           (produced_by IN ('ingest','external_observed'))
        OR produced_from_version_id IS NOT NULL
    )
) STRICT;

INSERT INTO file_versions
SELECT id, file_asset_id, content_hash, size_bytes, produced_by,
       produced_from_version_id, created_at, retired_at, epoch
FROM file_versions_old;

DROP TABLE file_versions_old;

CREATE INDEX file_versions_by_asset ON file_versions (file_asset_id);
CREATE INDEX file_versions_by_hash  ON file_versions (content_hash);

PRAGMA legacy_alter_table = OFF;
PRAGMA foreign_keys = ON;

PRAGMA foreign_key_check;

BEGIN;
