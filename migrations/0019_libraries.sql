-- Sprint 17 -- library and library-root configuration (T11).
--
-- Durable operator configuration for what a future daemon is allowed to
-- observe. A `library` groups roots and default policy intent; a
-- `library_root` is one canonical path with discovery/scan settings. Explicit
-- CLI scans still take an arbitrary path; `voom scan --root` and the watcher
-- read only enabled roots. See docs/adr/0027 and the design-doc
-- §Library Configuration Model.
--
-- `canonical_path` is UNIQUE across all roots so two roots cannot claim one
-- physical path; `voom library root add` additionally refuses a path that is a
-- component-wise ancestor-or-descendant of an existing root. Cross-issue
-- defaults (scheduling/safety policy #281, scoring profile #285,
-- external-system links #284, last scan session) are added as columns by their
-- owning issues, not stubbed here.

CREATE TABLE libraries (
    id           INTEGER PRIMARY KEY,
    slug         TEXT NOT NULL,
    display_name TEXT NOT NULL,
    media_kind   TEXT NOT NULL
        CHECK (media_kind IN ('movie', 'episode', 'personal', 'unknown')),
    description  TEXT,
    enabled      INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
) STRICT;

CREATE UNIQUE INDEX libraries_slug ON libraries (slug);

CREATE TABLE library_roots (
    id                   INTEGER PRIMARY KEY,
    library_id           INTEGER NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    root_kind            TEXT NOT NULL
        CHECK (root_kind IN ('local_path', 'shared_mount')),
    canonical_path       TEXT NOT NULL,
    display_path         TEXT NOT NULL,
    include_globs        TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(include_globs)),
    exclude_globs        TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(exclude_globs)),
    extension_allowlist  TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(extension_allowlist)),
    scan_mode            TEXT NOT NULL
        CHECK (scan_mode IN ('explicit_only', 'manual_recursive', 'watch_enabled')),
    symlink_policy       TEXT NOT NULL CHECK (symlink_policy IN ('reject', 'follow')),
    hidden_file_policy   TEXT NOT NULL CHECK (hidden_file_policy IN ('ignore', 'include')),
    max_depth            INTEGER CHECK (max_depth IS NULL OR max_depth >= 0),
    stability_seconds    INTEGER NOT NULL DEFAULT 0 CHECK (stability_seconds >= 0),
    debounce_seconds     INTEGER NOT NULL DEFAULT 0 CHECK (debounce_seconds >= 0),
    default_output_root  TEXT,
    default_staging_root TEXT,
    default_backup_root  TEXT,
    enabled              INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL
) STRICT;

CREATE INDEX library_roots_by_library ON library_roots (library_id, id);
CREATE UNIQUE INDEX library_roots_canonical_path ON library_roots (canonical_path);
