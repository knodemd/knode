-- knode.db schema version 3, format 0: space settings and folders, agents, keys and grants,
-- and change history and undo.
-- The normative description is in the .knode specification v0:
-- https://github.com/knodemd/format/blob/main/spec/knode-0.md

-- Space settings and folders (section 15).
CREATE TABLE folders (
    id         TEXT PRIMARY KEY NOT NULL CHECK (length(id) = 26),
    kind       TEXT NOT NULL,
    path       TEXT NOT NULL,
    capture    TEXT NOT NULL DEFAULT 'on' CHECK (capture IN ('on', 'off')),
    rules      TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL,
    created_by INTEGER NOT NULL REFERENCES provenance (id),
    updated_at INTEGER NOT NULL,
    updated_by INTEGER NOT NULL REFERENCES provenance (id),
    UNIQUE (kind, path)
) STRICT;

-- Agents, keys and grants (section 16). Keys themselves are never stored here.
CREATE TABLE agents (
    id         TEXT PRIMARY KEY NOT NULL CHECK (length(id) = 26),
    name       TEXT NOT NULL UNIQUE,
    kind       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    created_by INTEGER NOT NULL REFERENCES provenance (id)
) STRICT;

CREATE TABLE agent_keys (
    fingerprint TEXT PRIMARY KEY NOT NULL
                CHECK (length(fingerprint) = 71 AND substr(fingerprint, 1, 7) = 'sha256:'),
    agent_id    TEXT NOT NULL REFERENCES agents (id),
    created_at  INTEGER NOT NULL,
    created_by  INTEGER NOT NULL REFERENCES provenance (id),
    revoked_at  INTEGER,
    revoked_by  INTEGER REFERENCES provenance (id)
) STRICT;

CREATE INDEX agent_keys_agent ON agent_keys (agent_id);

CREATE TABLE grants (
    agent_id   TEXT PRIMARY KEY NOT NULL REFERENCES agents (id),
    level      TEXT NOT NULL CHECK (level IN ('none', 'read', 'write')),
    updated_at INTEGER NOT NULL,
    updated_by INTEGER NOT NULL REFERENCES provenance (id)
) STRICT;

-- Change history and undo (section 17).
CREATE TABLE changes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    changed_at INTEGER NOT NULL,
    changed_by INTEGER NOT NULL REFERENCES provenance (id),
    action     TEXT NOT NULL,
    subject    TEXT NOT NULL DEFAULT '',
    undoes     INTEGER REFERENCES changes (id),
    detail     TEXT NOT NULL DEFAULT '{}'
) STRICT;

CREATE INDEX changes_subject ON changes (subject, id);
CREATE INDEX changes_undoes ON changes (undoes) WHERE undoes IS NOT NULL;
CREATE INDEX changes_changed_at ON changes (changed_at);

ALTER TABLE history ADD COLUMN change_id INTEGER REFERENCES changes (id);
ALTER TABLE history ADD COLUMN restored_from INTEGER REFERENCES history (seq);

CREATE INDEX history_change ON history (change_id);

CREATE TABLE link_history (
    seq       INTEGER PRIMARY KEY AUTOINCREMENT,
    change_id INTEGER NOT NULL REFERENCES changes (id),
    op        TEXT NOT NULL CHECK (op IN ('create', 'delete')),
    source_id TEXT NOT NULL,
    kind      TEXT NOT NULL,
    target_id TEXT NOT NULL
) STRICT;

CREATE INDEX link_history_change ON link_history (change_id);
CREATE INDEX link_history_source ON link_history (source_id, seq);
CREATE INDEX link_history_target ON link_history (target_id, seq);

-- Every version written before schema version 3 becomes a change of its own,
-- with the same number as its history row.
INSERT INTO changes (id, changed_at, changed_by, action, subject)
SELECT seq, changed_at, changed_by, 'node.' || op, node_id FROM history ORDER BY seq;

UPDATE history SET change_id = seq;
