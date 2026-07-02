-- Per-location hardlink inode facts captured at scan time (#249).
--
-- One row per ingested local file_location, keyed 1:1 by that location.
-- (dev, ino) is the physical-object key: two file_locations with the same
-- (dev, ino) are hardlinks to one physical file. nlink is recorded for operator
-- visibility (a value > 1 means the physical file has other links). dev/ino are
-- stored as the signed reinterpretation of the OS-reported u64 identifiers.
CREATE TABLE scan_file_facts (
    file_location_id INTEGER PRIMARY KEY
        REFERENCES file_locations (id) ON DELETE CASCADE,
    dev              INTEGER NOT NULL,
    ino              INTEGER NOT NULL,
    nlink            INTEGER NOT NULL,
    observed_at      TEXT NOT NULL
) STRICT;

-- Hardlink lookup: find prior live locations sharing a physical object. Non-
-- unique because a recycled inode can legitimately produce several rows over
-- time; the resolver joins only live locations and confirms content identity.
CREATE INDEX idx_scan_file_facts_dev_ino ON scan_file_facts (dev, ino);
