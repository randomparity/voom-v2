-- Issue #418: replace globally meaningful root/location paths with node-owned
-- roots and provider-relative file locations.
--
-- SQLite cannot apply the foreign-key and legacy-alter-table PRAGMAs while
-- sqlx's migration transaction is open. Follow the established 0012/0013
-- rebuild protocol: commit that wrapper, change the session PRAGMAs, rebuild
-- while preserving numeric IDs, validate, then begin a transaction for sqlx's
-- migration bookkeeping. A failure leaves the migration dirty and requires
-- restore from the pre-upgrade database backup.
COMMIT;
PRAGMA foreign_keys = OFF;
PRAGMA legacy_alter_table = ON;

CREATE TEMP TABLE migration_0034_count_guard (
    ok INTEGER NOT NULL CHECK (ok = 1)
) STRICT;

-- The staging path originally used a shorter lineage key than the media
-- operation paths. Refuse ambiguous rows, then replace that legacy spelling
-- in one JSON update so every artifact handle retains its exact source
-- FileLocation under one durable vocabulary.
INSERT INTO migration_0034_count_guard (ok)
SELECT NOT EXISTS (
    SELECT 1
    FROM artifact_handles
    WHERE json_type(source_lineage, '$.source_location_id') IS NOT NULL
      AND json_type(source_lineage, '$.source_file_location_id') IS NOT NULL
);
DELETE FROM migration_0034_count_guard;

UPDATE artifact_handles
SET source_lineage = json_remove(
    json_set(
        source_lineage,
        '$.source_file_location_id',
        json_extract(source_lineage, '$.source_location_id')
    ),
    '$.source_location_id'
)
WHERE json_type(source_lineage, '$.source_location_id') IS NOT NULL;

ALTER TABLE library_roots RENAME TO library_roots_old;

CREATE TABLE library_roots (
    id                       INTEGER PRIMARY KEY,
    library_id               INTEGER NOT NULL
        REFERENCES libraries(id) ON DELETE RESTRICT,
    owner_node_id            INTEGER REFERENCES nodes(id) ON DELETE RESTRICT,
    provider_kind            TEXT NOT NULL
        CHECK (provider_kind IN ('local_filesystem')),
    provider_locator         TEXT NOT NULL
        CHECK (length(CAST(provider_locator AS BLOB)) BETWEEN 1 AND 4096)
        CHECK (instr(provider_locator, char(0)) = 0),
    display_locator          TEXT NOT NULL,
    state                    TEXT NOT NULL
        CHECK (state IN ('unassigned', 'configured', 'active', 'unavailable', 'retired')),
    root_epoch               INTEGER NOT NULL DEFAULT 0 CHECK (root_epoch >= 0),
    activation_identity      TEXT CHECK (
        activation_identity IS NULL
        OR (
            length(CAST(activation_identity AS BLOB)) BETWEEN 1 AND 4096
            AND instr(activation_identity, char(0)) = 0
        )
    ),
    include_globs            TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(include_globs)),
    exclude_globs            TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(exclude_globs)),
    extension_allowlist      TEXT NOT NULL DEFAULT '[]'
        CHECK (json_valid(extension_allowlist)),
    scan_mode                TEXT NOT NULL
        CHECK (scan_mode IN ('explicit_only', 'manual_recursive', 'watch_enabled')),
    symlink_policy           TEXT NOT NULL CHECK (symlink_policy IN ('reject', 'follow')),
    hidden_file_policy       TEXT NOT NULL CHECK (hidden_file_policy IN ('ignore', 'include')),
    max_depth                INTEGER CHECK (max_depth IS NULL OR max_depth >= 0),
    stability_seconds        INTEGER NOT NULL DEFAULT 0 CHECK (stability_seconds >= 0),
    debounce_seconds         INTEGER NOT NULL DEFAULT 0 CHECK (debounce_seconds >= 0),
    default_output_root_id   INTEGER REFERENCES library_roots(id) ON DELETE RESTRICT,
    default_staging_root_id  INTEGER REFERENCES library_roots(id) ON DELETE RESTRICT,
    default_backup_root_id   INTEGER REFERENCES library_roots(id) ON DELETE RESTRICT,
    enabled                  INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    created_at               TEXT NOT NULL,
    updated_at               TEXT NOT NULL,
    CHECK (
        (state = 'unassigned' AND owner_node_id IS NULL AND activation_identity IS NULL)
        OR
        (state = 'configured' AND owner_node_id IS NOT NULL AND activation_identity IS NULL)
        OR
        (state IN ('active', 'unavailable')
         AND owner_node_id IS NOT NULL AND activation_identity IS NOT NULL)
        OR
        (state = 'retired'
         AND (owner_node_id IS NOT NULL OR activation_identity IS NULL))
    )
) STRICT;

INSERT INTO library_roots (
    id,
    library_id,
    owner_node_id,
    provider_kind,
    provider_locator,
    display_locator,
    state,
    root_epoch,
    activation_identity,
    include_globs,
    exclude_globs,
    extension_allowlist,
    scan_mode,
    symlink_policy,
    hidden_file_policy,
    max_depth,
    stability_seconds,
    debounce_seconds,
    default_output_root_id,
    default_staging_root_id,
    default_backup_root_id,
    enabled,
    created_at,
    updated_at
)
SELECT
    id,
    library_id,
    NULL,
    'local_filesystem',
    canonical_path,
    display_path,
    'unassigned',
    0,
    NULL,
    include_globs,
    exclude_globs,
    extension_allowlist,
    scan_mode,
    symlink_policy,
    hidden_file_policy,
    max_depth,
    stability_seconds,
    debounce_seconds,
    NULL,
    NULL,
    NULL,
    0,
    created_at,
    updated_at
FROM library_roots_old;

INSERT INTO migration_0034_count_guard (ok)
SELECT count(*) = (SELECT count(*) FROM library_roots_old)
FROM library_roots;
DELETE FROM migration_0034_count_guard;

DROP TABLE library_roots_old;

CREATE INDEX library_roots_by_library ON library_roots (library_id, id);
CREATE INDEX library_roots_by_owner_state
    ON library_roots (owner_node_id, state, id)
    WHERE owner_node_id IS NOT NULL;
CREATE UNIQUE INDEX library_roots_owner_provider_locator
    ON library_roots (owner_node_id, provider_kind, provider_locator)
    WHERE owner_node_id IS NOT NULL AND state != 'retired';

ALTER TABLE file_locations RENAME TO file_locations_old;

CREATE TABLE file_locations (
    id                            INTEGER PRIMARY KEY,
    file_version_id               INTEGER NOT NULL
        REFERENCES file_versions(id) ON DELETE RESTRICT,
    address_state                 TEXT NOT NULL
        CHECK (address_state IN ('rooted', 'unassigned_legacy')),
    storage_root_id               INTEGER
        REFERENCES library_roots(id) ON DELETE RESTRICT,
    provider_relative_locator     TEXT,
    legacy_kind                   TEXT CHECK (
        legacy_kind IS NULL OR legacy_kind IN (
            'local_path', 'shared_mount', 'object_store_key', 'backup_path', 'historical'
        )
    ),
    legacy_locator                TEXT,
    proof_kind                    TEXT CHECK (
        proof_kind IS NULL OR proof_kind IN ('file_id_generation', 'object_version_id')
    ),
    proof_value                   TEXT,
    observed_at                   TEXT NOT NULL,
    retired_at                    TEXT,
    epoch                         INTEGER NOT NULL DEFAULT 0 CHECK (epoch >= 0),
    CHECK (
        (address_state = 'rooted'
         AND storage_root_id IS NOT NULL
         AND provider_relative_locator IS NOT NULL
         AND legacy_kind IS NULL
         AND legacy_locator IS NULL)
        OR
        (address_state = 'unassigned_legacy'
         AND storage_root_id IS NULL
         AND provider_relative_locator IS NULL
         AND legacy_kind IS NOT NULL
         AND legacy_locator IS NOT NULL)
    ),
    CHECK (
        provider_relative_locator IS NULL
        OR (
            length(CAST(provider_relative_locator AS BLOB)) BETWEEN 1 AND 4096
            AND instr(provider_relative_locator, char(0)) = 0
            AND instr(provider_relative_locator, '\') = 0
            AND substr(provider_relative_locator, 1, 1) != '/'
            AND substr(provider_relative_locator, -1, 1) != '/'
            AND instr(provider_relative_locator, '//') = 0
            AND instr('/' || provider_relative_locator || '/', '/./') = 0
            AND instr('/' || provider_relative_locator || '/', '/../') = 0
        )
    ),
    CHECK (
        (proof_kind IS NULL AND proof_value IS NULL)
        OR (proof_kind IS NOT NULL AND proof_value IS NOT NULL)
    )
) STRICT;

INSERT INTO file_locations (
    id,
    file_version_id,
    address_state,
    storage_root_id,
    provider_relative_locator,
    legacy_kind,
    legacy_locator,
    proof_kind,
    proof_value,
    observed_at,
    retired_at,
    epoch
)
SELECT
    id,
    file_version_id,
    'unassigned_legacy',
    NULL,
    NULL,
    kind,
    value,
    proof_kind,
    proof_value,
    observed_at,
    retired_at,
    epoch
FROM file_locations_old;

INSERT INTO migration_0034_count_guard (ok)
SELECT count(*) = (SELECT count(*) FROM file_locations_old)
FROM file_locations;
DELETE FROM migration_0034_count_guard;

DROP TABLE file_locations_old;

CREATE INDEX file_locations_by_version ON file_locations (file_version_id);
CREATE INDEX file_locations_live
    ON file_locations (file_version_id) WHERE retired_at IS NULL;
CREATE INDEX file_locations_by_root
    ON file_locations (storage_root_id, id) WHERE storage_root_id IS NOT NULL;
CREATE UNIQUE INDEX file_locations_live_rooted_address
    ON file_locations (storage_root_id, provider_relative_locator)
    WHERE address_state = 'rooted' AND retired_at IS NULL;

DROP TABLE migration_0034_count_guard;

PRAGMA legacy_alter_table = OFF;
PRAGMA foreign_keys = ON;

PRAGMA foreign_key_check;

BEGIN;
