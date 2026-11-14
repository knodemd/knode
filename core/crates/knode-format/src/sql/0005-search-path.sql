-- knode.db schema version 5, format 0: search imported Markdown paths.
-- Only derived search data changes; nodes, properties and history stay as they were.
DROP TRIGGER nodes_fts_insert;
DROP TRIGGER nodes_fts_delete;
DROP TRIGGER nodes_fts_update;
DROP TABLE nodes_fts;

CREATE VIEW nodes_fts_content AS
SELECT n.seq, n.id, n.title, n.plain, coalesce(p.value, '') AS markdown_path
FROM nodes n LEFT JOIN properties p ON p.node_id = n.id AND p.key = 'markdown.path';

CREATE VIRTUAL TABLE nodes_fts USING fts5 (
    title, plain, markdown_path,
    content = 'nodes_fts_content', content_rowid = 'seq',
    tokenize = 'unicode61 remove_diacritics 2',
    prefix = '2 3'
);
INSERT INTO nodes_fts (nodes_fts, rank) VALUES ('rank', 'bm25(5.0, 1.0, 2.0)');
INSERT INTO nodes_fts (nodes_fts) VALUES ('rebuild');

CREATE TRIGGER nodes_fts_insert AFTER INSERT ON nodes BEGIN
    INSERT INTO nodes_fts (rowid, title, plain, markdown_path)
    SELECT seq, title, plain, markdown_path FROM nodes_fts_content WHERE id = new.id;
END;

-- Delete before the foreign-key cascade removes the path property.
CREATE TRIGGER nodes_fts_delete BEFORE DELETE ON nodes BEGIN
    INSERT INTO nodes_fts (nodes_fts, rowid, title, plain, markdown_path)
    SELECT 'delete', seq, title, plain, markdown_path FROM nodes_fts_content WHERE id = old.id;
END;

CREATE TRIGGER nodes_fts_update AFTER UPDATE OF title, plain ON nodes BEGIN
    INSERT INTO nodes_fts (nodes_fts, rowid, title, plain, markdown_path)
    SELECT 'delete', old.seq, old.title, old.plain, markdown_path
    FROM nodes_fts_content WHERE id = new.id;
    INSERT INTO nodes_fts (rowid, title, plain, markdown_path)
    SELECT seq, title, plain, markdown_path FROM nodes_fts_content WHERE id = new.id;
END;

-- An AFTER INSERT trigger avoids deleting twice during INSERT ... ON CONFLICT UPDATE.
CREATE TRIGGER properties_fts_insert AFTER INSERT ON properties
WHEN new.key = 'markdown.path' BEGIN
    INSERT INTO nodes_fts (nodes_fts, rowid, title, plain, markdown_path)
    SELECT 'delete', seq, title, plain, '' FROM nodes_fts_content WHERE id = new.node_id;
    INSERT INTO nodes_fts (rowid, title, plain, markdown_path)
    SELECT seq, title, plain, markdown_path FROM nodes_fts_content WHERE id = new.node_id;
END;

CREATE TRIGGER properties_fts_before_delete BEFORE DELETE ON properties
WHEN old.key = 'markdown.path' BEGIN
    INSERT INTO nodes_fts (nodes_fts, rowid, title, plain, markdown_path)
    SELECT 'delete', seq, title, plain, markdown_path FROM nodes_fts_content WHERE id = old.node_id;
END;

CREATE TRIGGER properties_fts_delete AFTER DELETE ON properties
WHEN old.key = 'markdown.path' BEGIN
    INSERT INTO nodes_fts (rowid, title, plain, markdown_path)
    SELECT seq, title, plain, markdown_path FROM nodes_fts_content WHERE id = old.node_id;
END;

CREATE TRIGGER properties_fts_before_update BEFORE UPDATE OF node_id, key, value ON properties
WHEN old.key = 'markdown.path' OR new.key = 'markdown.path' BEGIN
    INSERT INTO nodes_fts (nodes_fts, rowid, title, plain, markdown_path)
    SELECT 'delete', seq, title, plain, markdown_path FROM nodes_fts_content
    WHERE id IN (old.node_id, new.node_id);
END;

CREATE TRIGGER properties_fts_update AFTER UPDATE OF node_id, key, value ON properties
WHEN old.key = 'markdown.path' OR new.key = 'markdown.path' BEGIN
    INSERT INTO nodes_fts (rowid, title, plain, markdown_path)
    SELECT seq, title, plain, markdown_path FROM nodes_fts_content
    WHERE id IN (old.node_id, new.node_id);
END;
