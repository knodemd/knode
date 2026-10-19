-- knode.db schema version 1, format 0.
-- The normative description of every table is in the .knode specification v0:
-- https://github.com/knodemd/format/blob/main/spec/knode-0.md

CREATE TABLE settings (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE provenance (
    id         INTEGER PRIMARY KEY,
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('person', 'agent', 'system')),
    actor      TEXT NOT NULL,
    session    TEXT NOT NULL DEFAULT '',
    source     TEXT NOT NULL DEFAULT '',
    UNIQUE (actor_kind, actor, session, source)
) STRICT;

CREATE TABLE nodes (
    seq        INTEGER PRIMARY KEY,
    id         TEXT NOT NULL UNIQUE CHECK (length(id) = 26),
    kind       TEXT NOT NULL DEFAULT 'note',
    title      TEXT NOT NULL,
    title_key  TEXT NOT NULL,
    body       TEXT NOT NULL DEFAULT '',
    plain      TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    created_by INTEGER NOT NULL REFERENCES provenance (id),
    updated_by INTEGER NOT NULL REFERENCES provenance (id)
) STRICT;

CREATE INDEX nodes_title_key ON nodes (title_key);
CREATE INDEX nodes_updated_at ON nodes (updated_at);
CREATE INDEX nodes_kind ON nodes (kind, updated_at);

CREATE TABLE tags (
    node_id TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    tag     TEXT NOT NULL,
    PRIMARY KEY (node_id, tag)
) STRICT, WITHOUT ROWID;

CREATE INDEX tags_tag ON tags (tag);

CREATE TABLE links (
    source_id  TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    target_id  TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    created_by INTEGER NOT NULL REFERENCES provenance (id),
    PRIMARY KEY (source_id, kind, target_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX links_target ON links (target_id, kind);

CREATE TABLE unresolved_links (
    source_id  TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    target_key TEXT NOT NULL,
    target     TEXT NOT NULL,
    PRIMARY KEY (source_id, target_key)
) STRICT, WITHOUT ROWID;

CREATE INDEX unresolved_links_target ON unresolved_links (target_key);

CREATE TABLE history (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id    TEXT NOT NULL,
    op         TEXT NOT NULL CHECK (op IN ('create', 'update', 'delete')),
    changed_at INTEGER NOT NULL,
    changed_by INTEGER NOT NULL REFERENCES provenance (id),
    kind       TEXT NOT NULL,
    title      TEXT NOT NULL,
    body       TEXT NOT NULL,
    tags       TEXT NOT NULL DEFAULT '[]'
) STRICT;

CREATE INDEX history_node ON history (node_id, seq);

CREATE VIRTUAL TABLE nodes_fts USING fts5 (
    title,
    plain,
    content = 'nodes',
    content_rowid = 'seq',
    tokenize = 'unicode61 remove_diacritics 2',
    prefix = '2 3'
);

-- Rank by BM25, a match in the title weighing five times one in the text.
INSERT INTO nodes_fts (nodes_fts, rank) VALUES ('rank', 'bm25(5.0, 1.0)');

CREATE TRIGGER nodes_fts_insert AFTER INSERT ON nodes BEGIN
    INSERT INTO nodes_fts (rowid, title, plain) VALUES (new.seq, new.title, new.plain);
END;

CREATE TRIGGER nodes_fts_delete AFTER DELETE ON nodes BEGIN
    INSERT INTO nodes_fts (nodes_fts, rowid, title, plain)
    VALUES ('delete', old.seq, old.title, old.plain);
END;

CREATE TRIGGER nodes_fts_update AFTER UPDATE OF title, plain ON nodes BEGIN
    INSERT INTO nodes_fts (nodes_fts, rowid, title, plain)
    VALUES ('delete', old.seq, old.title, old.plain);
    INSERT INTO nodes_fts (rowid, title, plain) VALUES (new.seq, new.title, new.plain);
END;
