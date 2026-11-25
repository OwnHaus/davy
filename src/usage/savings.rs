//! Where logs and errors could take less (`docs/design.md`, "Savings"):
//! the patterns with the most bytes and when a search last asked for each,
//! the log retention each project's searches need, and the issues that
//! send most of a project's error events.

/// The discard mark of an issue, the events discarded per day, the log
/// searches people and agents ran, and per day the searches and the
/// oldest read, of a project or of every project (`project_id` 0). One
/// migration, so a branch that takes the same number renumbers one entry.
pub const MIGRATION: &str = "ALTER TABLE issues ADD COLUMN discarded_at INTEGER;
CREATE TABLE discarded_daily(day TEXT NOT NULL, project_id INTEGER NOT NULL REFERENCES projects(id), records INTEGER NOT NULL, PRIMARY KEY(day, project_id)) WITHOUT ROWID;
CREATE TABLE searches(id INTEGER PRIMARY KEY, at INTEGER NOT NULL, by TEXT NOT NULL, project_id INTEGER, since INTEGER NOT NULL, until INTEGER NOT NULL, level INTEGER, pattern INTEGER, query TEXT);
CREATE TABLE search_days(day TEXT NOT NULL, project_id INTEGER NOT NULL, searches INTEGER NOT NULL, oldest_read_days INTEGER NOT NULL, PRIMARY KEY(day, project_id)) WITHOUT ROWID;";
/// Undoes [`MIGRATION`], for tests that run the migrations again,
/// examples included.
pub const UNDO: &str = "ALTER TABLE issues DROP COLUMN discarded_at; DROP TABLE discarded_daily; DROP TABLE searches; DROP TABLE search_days;";
