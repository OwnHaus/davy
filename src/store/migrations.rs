/// Schema changes after the first release, applied in order. `PRAGMA
/// user_version` counts how many ran.
pub(super) const MIGRATIONS: [&str; 14] = [
    "CREATE TABLE releases(project_id INTEGER NOT NULL REFERENCES projects(id), version TEXT NOT NULL, first_seen INTEGER NOT NULL, PRIMARY KEY(project_id, version));
INSERT INTO releases SELECT project_id, release, min(received_at) FROM events WHERE release IS NOT NULL GROUP BY project_id, release;",
    "CREATE TABLE monitors(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id), name TEXT NOT NULL, kind TEXT NOT NULL, url TEXT, expect TEXT, keyword TEXT, heartbeat_token TEXT UNIQUE, every_s INTEGER NOT NULL, timeout_s INTEGER, grace_s INTEGER, state TEXT NOT NULL, last_check_at INTEGER, last_ping_at INTEGER, created_at INTEGER NOT NULL);
CREATE TABLE checks(monitor_id INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE, at INTEGER NOT NULL, ok INTEGER NOT NULL, status INTEGER, ms INTEGER, reason TEXT);
CREATE INDEX checks_monitor_at ON checks(monitor_id, at);
CREATE TABLE incidents(id INTEGER PRIMARY KEY, monitor_id INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE, started_at INTEGER NOT NULL, resolved_at INTEGER, reason TEXT NOT NULL);
CREATE INDEX incidents_open ON incidents(monitor_id) WHERE resolved_at IS NULL;",
    "CREATE TABLE channels(id INTEGER PRIMARY KEY, project_id INTEGER REFERENCES projects(id), kind TEXT NOT NULL, url TEXT NOT NULL, secret TEXT, dropped INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL);
CREATE TABLE notifications(id INTEGER PRIMARY KEY, channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE, type TEXT NOT NULL, payload TEXT NOT NULL, created_at INTEGER NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, next_at INTEGER NOT NULL, sent_at INTEGER, failed_at INTEGER, last_error TEXT);
CREATE INDEX notifications_pending ON notifications(channel_id, id) WHERE sent_at IS NULL AND failed_at IS NULL;",
    "CREATE TABLE feedback(id INTEGER PRIMARY KEY, at INTEGER NOT NULL, client TEXT, tried TEXT NOT NULL, happened TEXT NOT NULL, fix TEXT);
ALTER TABLE channels ADD COLUMN feedback INTEGER NOT NULL DEFAULT 0;",
    "CREATE TABLE users(id INTEGER PRIMARY KEY, email TEXT UNIQUE NOT NULL, password TEXT, created_at INTEGER NOT NULL);
CREATE TABLE sessions(hash BLOB PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, created_at INTEGER NOT NULL, used_at INTEGER NOT NULL);
CREATE INDEX sessions_user ON sessions(user_id);
CREATE TABLE invites(hash BLOB PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, expires_at INTEGER NOT NULL);
CREATE INDEX invites_user ON invites(user_id);",
    "CREATE INDEX IF NOT EXISTS issues_project_status ON issues(project_id, status, last_seen DESC);",
    // The issues list. Each index holds every sort key, so a list in any
    // order with any status reads only the index, and two indexes instead of
    // nine keep ingest, which updates last_seen and event_count, cheap.
    // issues_text and issue_tags turn the text, environment and release
    // filters into sets of ids instead of a scan. Timings in docs/design.md,
    // "Load check".
    "DROP INDEX IF EXISTS issues_last_seen;
DROP INDEX IF EXISTS issues_project_status;
CREATE INDEX issues_status_sorts ON issues(status, last_seen, event_count, first_seen);
CREATE INDEX issues_project_sorts ON issues(project_id, status, last_seen, event_count, first_seen);
CREATE VIRTUAL TABLE issues_text USING fts5(title, culprit, content='issues', content_rowid='id', tokenize='trigram');
INSERT INTO issues_text(issues_text) VALUES('rebuild');
CREATE TRIGGER issues_text_insert AFTER INSERT ON issues BEGIN INSERT INTO issues_text(rowid, title, culprit) VALUES (new.id, new.title, new.culprit); END;
CREATE TRIGGER issues_text_update AFTER UPDATE OF title, culprit ON issues BEGIN INSERT INTO issues_text(issues_text, rowid, title, culprit) VALUES ('delete', old.id, old.title, old.culprit); INSERT INTO issues_text(rowid, title, culprit) VALUES (new.id, new.title, new.culprit); END;
CREATE TRIGGER issues_text_delete AFTER DELETE ON issues BEGIN INSERT INTO issues_text(issues_text, rowid, title, culprit) VALUES ('delete', old.id, old.title, old.culprit); END;
CREATE TABLE issue_tags(key TEXT NOT NULL CHECK(key IN ('environment', 'release')), value TEXT NOT NULL, issue_id INTEGER NOT NULL REFERENCES issues(id), PRIMARY KEY(key, value, issue_id)) WITHOUT ROWID;
INSERT OR IGNORE INTO issue_tags SELECT 'environment', environment, issue_id FROM events WHERE environment IS NOT NULL;
INSERT OR IGNORE INTO issue_tags SELECT 'release', release, issue_id FROM events WHERE release IS NOT NULL;
CREATE TRIGGER events_tags AFTER INSERT ON events BEGIN INSERT OR IGNORE INTO issue_tags SELECT 'environment', new.environment, new.issue_id WHERE new.environment IS NOT NULL; INSERT OR IGNORE INTO issue_tags SELECT 'release', new.release, new.issue_id WHERE new.release IS NOT NULL; END;",
    // What the setup page shows that no other table holds.
    "CREATE TABLE firsts(what TEXT PRIMARY KEY, at INTEGER NOT NULL);",
    // Client reports and user feedback from the SDKs, see src/items.
    "CREATE TABLE client_reports(day INTEGER NOT NULL, project_id INTEGER NOT NULL REFERENCES projects(id), reason TEXT NOT NULL, category TEXT NOT NULL, quantity INTEGER NOT NULL, PRIMARY KEY(day, project_id, reason, category)) WITHOUT ROWID;
CREATE TABLE user_feedback(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id), received_at INTEGER NOT NULL, event_id TEXT, issue_id INTEGER REFERENCES issues(id), message TEXT NOT NULL, name TEXT, email TEXT, url TEXT);
CREATE INDEX user_feedback_project ON user_feedback(project_id, received_at);
CREATE INDEX user_feedback_issue ON user_feedback(issue_id, received_at) WHERE issue_id IS NOT NULL;",
    // Release health. session_seen counts each session once however many
    // updates its SDK sends. release_users.user is a hash of the SDK's user id.
    "CREATE TABLE release_sessions(project_id INTEGER NOT NULL REFERENCES projects(id), hour INTEGER NOT NULL, release TEXT NOT NULL, environment TEXT NOT NULL, started INTEGER NOT NULL, errored INTEGER NOT NULL, crashed INTEGER NOT NULL, abnormal INTEGER NOT NULL, PRIMARY KEY(project_id, hour, release, environment)) WITHOUT ROWID;
CREATE TABLE release_users(day INTEGER NOT NULL, project_id INTEGER NOT NULL REFERENCES projects(id), release TEXT NOT NULL, environment TEXT NOT NULL, user INTEGER NOT NULL, flags INTEGER NOT NULL, PRIMARY KEY(day, project_id, release, environment, user)) WITHOUT ROWID;
CREATE TABLE session_seen(project_id INTEGER NOT NULL, sid BLOB NOT NULL, flags INTEGER NOT NULL, started INTEGER NOT NULL, PRIMARY KEY(project_id, sid)) WITHOUT ROWID;
CREATE INDEX session_seen_started ON session_seen(started);",
    // The issue an issue notice reports, so the issue notices that wait
    // together go out as one digest, see src/alert/digest.rs.
    "ALTER TABLE notifications ADD COLUMN issue TEXT;",
    crate::monitor::checkin_store::MIGRATION,
    // Notices of monitors made by check-ins that a full outbox dropped, kept
    // apart from `dropped` because anyone with a public DSN can raise them.
    "ALTER TABLE channels ADD COLUMN sdk_dropped INTEGER NOT NULL DEFAULT 0;",
    // Retention per project, and what deleting old events needs.
    crate::retention::MIGRATION,
];
