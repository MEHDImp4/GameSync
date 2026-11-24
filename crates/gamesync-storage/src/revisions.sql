PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS revisions (
    id INTEGER PRIMARY KEY,
    game TEXT NOT NULL,
    machine TEXT NOT NULL,
    request TEXT NOT NULL,
    base INTEGER REFERENCES revisions(id),
    manifest TEXT NOT NULL,
    published INTEGER NOT NULL CHECK (published IN (0, 1)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (game, request)
);

CREATE TABLE IF NOT EXISTS heads (
    game TEXT PRIMARY KEY,
    revision INTEGER NOT NULL REFERENCES revisions(id)
);

CREATE TABLE IF NOT EXISTS games (
    id TEXT PRIMARY KEY,
    registration TEXT NOT NULL
);
