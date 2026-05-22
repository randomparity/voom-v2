-- Sprint 4 - Durable policy documents and immutable policy versions.

CREATE TABLE policy_documents (
    id                          INTEGER PRIMARY KEY,
    slug                        TEXT NOT NULL UNIQUE,
    display_name                TEXT NOT NULL,
    created_at                  TEXT NOT NULL,
    current_accepted_version_id INTEGER,
    epoch                       INTEGER NOT NULL DEFAULT 0,
    CHECK (length(trim(slug)) > 0),
    CHECK (slug NOT GLOB '*[^a-z0-9_-]*'),
    CHECK (length(trim(display_name)) > 0),
    CHECK (epoch >= 0)
) STRICT;

CREATE TABLE policy_versions (
    id                 INTEGER PRIMARY KEY,
    policy_document_id INTEGER NOT NULL REFERENCES policy_documents(id) ON DELETE RESTRICT,
    version_number     INTEGER NOT NULL,
    source_text        TEXT NOT NULL,
    source_hash        TEXT NOT NULL,
    schema_version     INTEGER NOT NULL,
    compiled_json      TEXT NOT NULL CHECK (json_valid(compiled_json)),
    created_at         TEXT NOT NULL,
    CHECK (version_number > 0),
    CHECK (length(source_hash) = 64),
    CHECK (source_hash NOT GLOB '*[^0-9a-f]*'),
    CHECK (schema_version > 0),
    UNIQUE (policy_document_id, version_number),
    UNIQUE (policy_document_id, source_hash),
    UNIQUE (policy_document_id, id)
) STRICT;

CREATE INDEX policy_documents_by_slug
    ON policy_documents (slug);

CREATE INDEX policy_versions_by_document
    ON policy_versions (policy_document_id, version_number);

CREATE TRIGGER policy_documents_current_version_same_document_insert
BEFORE INSERT ON policy_documents
WHEN NEW.current_accepted_version_id IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'policy current version must belong to document')
    WHERE NOT EXISTS (
        SELECT 1 FROM policy_versions
        WHERE id = NEW.current_accepted_version_id
          AND policy_document_id = NEW.id
    );
END;

CREATE TRIGGER policy_documents_current_version_same_document_update
BEFORE UPDATE OF current_accepted_version_id ON policy_documents
WHEN NEW.current_accepted_version_id IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'policy current version must belong to document')
    WHERE NOT EXISTS (
        SELECT 1 FROM policy_versions
        WHERE id = NEW.current_accepted_version_id
          AND policy_document_id = NEW.id
    );
END;

CREATE TRIGGER policy_versions_are_immutable
BEFORE UPDATE ON policy_versions
BEGIN
    SELECT RAISE(ABORT, 'policy versions are immutable');
END;

CREATE TRIGGER policy_versions_are_not_deleted
BEFORE DELETE ON policy_versions
BEGIN
    SELECT RAISE(ABORT, 'policy versions are immutable');
END;
