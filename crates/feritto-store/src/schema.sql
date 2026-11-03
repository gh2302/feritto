BEGIN IMMEDIATE;
CREATE TABLE groups (
 id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES groups(id), name TEXT NOT NULL,
 metadata TEXT NOT NULL DEFAULT '{}'
);
CREATE TABLE clips (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 captured_at INTEGER NOT NULL, last_used INTEGER, content_hash TEXT NOT NULL UNIQUE,
 preview TEXT NOT NULL, search_text TEXT NOT NULL, search_truncated INTEGER NOT NULL DEFAULT 0,
 size INTEGER NOT NULL, source TEXT NOT NULL, complete INTEGER NOT NULL DEFAULT 1,
 omitted TEXT NOT NULL DEFAULT '[]', has_image INTEGER NOT NULL DEFAULT 0,
 starred INTEGER NOT NULL DEFAULT 0, protection_at INTEGER,
 group_id INTEGER REFERENCES groups(id), global_order REAL, group_order REAL,
 sticky_global REAL, sticky_group REAL, shortcut INTEGER, metadata TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX clips_recent ON clips(starred DESC,captured_at DESC,id DESC);
CREATE TABLE representations (
 clip_id INTEGER NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
 target TEXT NOT NULL, mime TEXT, inline BLOB, payload TEXT, size INTEGER NOT NULL,
 provenance TEXT NOT NULL DEFAULT 'native', PRIMARY KEY(clip_id,target),
 CHECK((inline IS NULL)!=(payload IS NULL))
);
CREATE INDEX representations_payload ON representations(payload);
CREATE TABLE assets (
 clip_id INTEGER NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
 kind TEXT NOT NULL, version INTEGER NOT NULL, bytes BLOB NOT NULL,
 PRIMARY KEY(clip_id,kind)
);
CREATE TABLE imports (source TEXT NOT NULL,source_id INTEGER NOT NULL,clip_id INTEGER REFERENCES clips(id),metadata TEXT NOT NULL,PRIMARY KEY(source,source_id));
CREATE VIRTUAL TABLE clips_fts USING fts5(search_text,content='clips',content_rowid='id',tokenize='trigram');
CREATE TRIGGER clips_ai AFTER INSERT ON clips BEGIN INSERT INTO clips_fts(rowid,search_text) VALUES(new.id,new.search_text); END;
CREATE TRIGGER clips_ad AFTER DELETE ON clips BEGIN INSERT INTO clips_fts(clips_fts,rowid,search_text) VALUES('delete',old.id,old.search_text); END;
CREATE TRIGGER clips_au AFTER UPDATE OF search_text ON clips BEGIN
 INSERT INTO clips_fts(clips_fts,rowid,search_text) VALUES('delete',old.id,old.search_text);
 INSERT INTO clips_fts(rowid,search_text) VALUES(new.id,new.search_text);
END;
PRAGMA user_version=1;
COMMIT;
