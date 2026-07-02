-- Sprint 17 -- T12 (#281): durable scheduling and safety policy records.
--
-- The pre-daemon safety baseline (design doc -> Security And Safety) forbids any
-- daemon loop from auto-scheduling real media mutation until operators can
-- configure and inspect these records from the CLI. Both tables are named,
-- slug-keyed configuration a future daemon reads rather than invents.
--
-- `schema_version` is the fail-closed staleness marker: the safety gate blocks
-- when a row's version does not match the reading binary's current version, so a
-- field added by a newer binary can never be silently defaulted (ADR 0028).
--
-- `auto_execute_operations` and `allowed_commit_modes` hold JSON arrays of scalar
-- enum wire-strings; the repository validates every element against the enum
-- vocabulary on write, so the DB never holds an unknown token.

CREATE TABLE scheduling_policies (
    id                     INTEGER PRIMARY KEY,
    slug                   TEXT NOT NULL UNIQUE,
    display_name           TEXT NOT NULL,
    schema_version         INTEGER NOT NULL CHECK (schema_version >= 1),
    priority               TEXT NOT NULL CHECK (priority IN (
                               'newest_first', 'oldest_first',
                               'smallest_first', 'largest_first')),
    copy_window            TEXT,
    large_jobs_night_only  INTEGER NOT NULL CHECK (large_jobs_night_only IN (0, 1)),
    pause_on_degraded_node INTEGER NOT NULL CHECK (pause_on_degraded_node IN (0, 1)),
    created_at             TEXT NOT NULL,
    updated_at             TEXT NOT NULL
) STRICT;

CREATE TABLE safety_policies (
    id                                 INTEGER PRIMARY KEY,
    slug                               TEXT NOT NULL UNIQUE,
    display_name                       TEXT NOT NULL,
    schema_version                     INTEGER NOT NULL CHECK (schema_version >= 1),
    auto_execute_operations            TEXT NOT NULL,
    backup_required                    INTEGER NOT NULL CHECK (backup_required IN (0, 1)),
    approval_required                  INTEGER NOT NULL CHECK (approval_required IN (0, 1)),
    allowed_commit_modes               TEXT NOT NULL,
    verification_level                 TEXT NOT NULL CHECK (verification_level IN (
                                           'none', 'quick_decode', 'full')),
    block_on_failed_records            INTEGER NOT NULL CHECK (block_on_failed_records IN (0, 1)),
    block_on_recovery_required_records INTEGER NOT NULL
                                           CHECK (block_on_recovery_required_records IN (0, 1)),
    created_at                         TEXT NOT NULL,
    updated_at                         TEXT NOT NULL
) STRICT;
