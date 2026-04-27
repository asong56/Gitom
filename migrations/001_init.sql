PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- Single-user Gitom: no users table, no oauth2 accounts.
-- Authentication is handled entirely via config/env vars.

CREATE TABLE IF NOT EXISTS repositories (
    id             INTEGER  PRIMARY KEY AUTOINCREMENT,
    name           TEXT     NOT NULL UNIQUE,
    description    TEXT,
    is_private     INTEGER  NOT NULL DEFAULT 0,
    default_branch TEXT              DEFAULT 'main',
    git_path       TEXT     NOT NULL,
    created_at     TEXT     NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at     TEXT     NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE TABLE IF NOT EXISTS webhooks (
    id         INTEGER  PRIMARY KEY AUTOINCREMENT,
    repo_id    INTEGER  NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    url        TEXT     NOT NULL,
    secret     TEXT,
    events     TEXT     NOT NULL DEFAULT '[]',
    is_active  INTEGER  NOT NULL DEFAULT 1,
    created_at TEXT     NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_webhooks_repo ON webhooks(repo_id);

CREATE TRIGGER IF NOT EXISTS repositories_updated_at
    AFTER UPDATE ON repositories
    BEGIN
        UPDATE repositories SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = NEW.id;
    END;

-- Single-row table that survives restarts. Seeded by the limiter on startup,
-- written on every failure / reset. Using a fixed id=1 row (upserted).
CREATE TABLE IF NOT EXISTS login_failures (
    id           INTEGER PRIMARY KEY CHECK (id = 1),
    count        INTEGER NOT NULL DEFAULT 0,
    window_start TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
INSERT OR IGNORE INTO login_failures (id, count, window_start)
    VALUES (1, 0, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));
