-- knode.db schema version 4, format 0: typed links and views, embeddings, attachments and
-- concurrent editing with Loro.
-- The normative description is in the .knode specification v0:
-- https://github.com/knodemd/format/blob/main/spec/knode-0.md

-- Typed links and views (section 18).
CREATE TABLE link_kinds (
    kind          TEXT PRIMARY KEY NOT NULL CHECK (length(kind) BETWEEN 1 AND 32),
    label         TEXT NOT NULL CHECK (length(label) BETWEEN 1 AND 64),
    inverse_label TEXT NOT NULL DEFAULT '' CHECK (length(inverse_label) <= 64),
    direction     TEXT NOT NULL DEFAULT 'directed' CHECK (direction IN ('directed', 'symmetric')),
    description   TEXT NOT NULL DEFAULT '',
    created_at    INTEGER NOT NULL,
    created_by    INTEGER NOT NULL REFERENCES provenance (id),
    updated_at    INTEGER NOT NULL,
    updated_by    INTEGER NOT NULL REFERENCES provenance (id)
) STRICT;

CREATE TABLE views (
    id         TEXT PRIMARY KEY NOT NULL CHECK (length(id) = 26),
    kind       TEXT NOT NULL,
    name       TEXT NOT NULL DEFAULT '',
    node_id    TEXT CHECK (node_id IS NULL OR length(node_id) = 26),
    settings   TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL,
    created_by INTEGER NOT NULL REFERENCES provenance (id),
    updated_at INTEGER NOT NULL,
    updated_by INTEGER NOT NULL REFERENCES provenance (id)
) STRICT;

CREATE UNIQUE INDEX views_node ON views (node_id) WHERE node_id IS NOT NULL;

CREATE TABLE view_groups (
    id         TEXT PRIMARY KEY NOT NULL CHECK (length(id) = 26),
    view_id    TEXT NOT NULL REFERENCES views (id) ON DELETE CASCADE,
    label      TEXT NOT NULL DEFAULT '',
    x          REAL NOT NULL,
    y          REAL NOT NULL,
    width      REAL NOT NULL CHECK (width > 0),
    height     REAL NOT NULL CHECK (height > 0),
    display    TEXT NOT NULL DEFAULT 'open' CHECK (display IN ('open', 'collapsed')),
    updated_at INTEGER NOT NULL,
    updated_by INTEGER NOT NULL REFERENCES provenance (id)
) STRICT;

CREATE INDEX view_groups_view ON view_groups (view_id);

CREATE TABLE positions (
    view_id    TEXT NOT NULL REFERENCES views (id) ON DELETE CASCADE,
    node_id    TEXT NOT NULL CHECK (length(node_id) = 26),
    x          REAL NOT NULL,
    y          REAL NOT NULL,
    width      REAL CHECK (width IS NULL OR width > 0),
    height     REAL CHECK (height IS NULL OR height > 0),
    z          INTEGER NOT NULL DEFAULT 0,
    display    TEXT NOT NULL DEFAULT 'open' CHECK (display IN ('open', 'collapsed')),
    group_id   TEXT REFERENCES view_groups (id) ON DELETE SET NULL,
    updated_at INTEGER NOT NULL,
    updated_by INTEGER NOT NULL REFERENCES provenance (id),
    PRIMARY KEY (view_id, node_id),
    CHECK ((width IS NULL) = (height IS NULL))
) STRICT, WITHOUT ROWID;

CREATE INDEX positions_node ON positions (node_id);
CREATE INDEX positions_group ON positions (group_id) WHERE group_id IS NOT NULL;

-- Embeddings for semantic search (section 19). The vector tables themselves
-- (embedding_vectors_<model id>, sqlite-vec) are created by the writer that
-- embeds, never by this migration (19.4).
CREATE TABLE embedding_models (
    id              INTEGER PRIMARY KEY,
    name            TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    revision        TEXT NOT NULL DEFAULT '',
    dimensions      INTEGER NOT NULL CHECK (dimensions BETWEEN 1 AND 8192),
    chunking        TEXT NOT NULL,
    query_prefix    TEXT NOT NULL DEFAULT '',
    document_prefix TEXT NOT NULL DEFAULT '',
    created_at      INTEGER NOT NULL,
    UNIQUE (name, revision, dimensions, chunking, query_prefix, document_prefix)
) STRICT;

CREATE TABLE embedding_chunks (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id     TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    model_id    INTEGER NOT NULL REFERENCES embedding_models (id),
    ord         INTEGER NOT NULL CHECK (ord >= 0),
    text_start  INTEGER NOT NULL CHECK (text_start >= 0),
    text_end    INTEGER NOT NULL CHECK (text_end >= text_start),
    heading     TEXT NOT NULL DEFAULT '',
    embedded_at INTEGER NOT NULL,
    UNIQUE (node_id, model_id, ord)
) STRICT;

CREATE INDEX embedding_chunks_model ON embedding_chunks (model_id, node_id);

-- A node whose title or text changes loses its chunks, so it is embedded again.
CREATE TRIGGER embedding_chunks_stale AFTER UPDATE OF title, plain ON nodes
WHEN old.title IS NOT new.title OR old.plain IS NOT new.plain BEGIN
    DELETE FROM embedding_chunks WHERE node_id = new.id;
END;

-- Attachments (section 20). The files are in blobs/, named by their hash.
CREATE TABLE blobs (
    hash       TEXT PRIMARY KEY NOT NULL
               CHECK (length(hash) = 64 AND hash NOT GLOB '*[^0-9a-f]*'),
    size       INTEGER NOT NULL CHECK (size >= 0),
    media_type TEXT NOT NULL DEFAULT 'application/octet-stream',
    created_at INTEGER NOT NULL,
    created_by INTEGER NOT NULL REFERENCES provenance (id)
) STRICT, WITHOUT ROWID;

CREATE TABLE attachments (
    node_id    TEXT NOT NULL CHECK (length(node_id) = 26),
    name       TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
    hash       TEXT NOT NULL REFERENCES blobs (hash),
    created_at INTEGER NOT NULL,
    created_by INTEGER NOT NULL REFERENCES provenance (id),
    PRIMARY KEY (node_id, name)
) STRICT, WITHOUT ROWID;

CREATE INDEX attachments_hash ON attachments (hash);

-- Concurrent editing with Loro (section 21). The documents are in sync/.
CREATE TABLE loro_docs (
    node_id    TEXT PRIMARY KEY NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    version    TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- A body written without Loro marks the node's document as behind (21.4).
CREATE TRIGGER loro_docs_body AFTER UPDATE OF body ON nodes
WHEN old.body IS NOT new.body BEGIN
    DELETE FROM loro_docs WHERE node_id = new.id;
END;

CREATE TABLE loro_peers (
    peer       TEXT PRIMARY KEY NOT NULL
               CHECK (length(peer) BETWEEN 1 AND 20 AND peer NOT GLOB '*[^0-9]*'),
    provenance INTEGER NOT NULL REFERENCES provenance (id),
    created_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
