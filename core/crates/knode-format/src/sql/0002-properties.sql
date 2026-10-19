-- knode.db schema version 2, format 0: properties of nodes.
-- The normative description is in the .knode specification v0:
-- https://github.com/knodemd/format/blob/main/spec/knode-0.md

CREATE TABLE properties (
    node_id    TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    updated_by INTEGER NOT NULL REFERENCES provenance (id),
    PRIMARY KEY (node_id, key)
) STRICT, WITHOUT ROWID;

CREATE INDEX properties_key_value ON properties (key, value);
