CREATE TABLE schema_meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;

INSERT INTO schema_meta (key, value)
VALUES ('schema_init_at', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
