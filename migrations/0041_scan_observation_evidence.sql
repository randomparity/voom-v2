-- Migration 0041 (physical version 3): scan observation evidence
-- (issue #421, ADR 0077).
--
-- Owner-node scan workers report typed evidence (agreed hash+probe facts,
-- sidecar digests, probe snapshot) alongside each observation. The payload is
-- strict JSON validated in Rust on every read and write
-- (`voom_core::ScanObservationEvidence`), so the column only enforces JSON
-- well-formedness here; NULL means the observation records existence without
-- publishing identity. Additive per ADR 0013.

ALTER TABLE scan_observations ADD COLUMN evidence_json TEXT
    CHECK (evidence_json IS NULL OR json_valid(evidence_json));
