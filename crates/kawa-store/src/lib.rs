//! SQLite storage, migrations and metric queries.

mod migrations;
mod sync_status;

use std::path::Path;

use rusqlite::Connection;
use thiserror::Error;

pub use sync_status::{SyncErrorKind, SyncStatus};

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("database schema version {found} is newer than this kawa supports ({supported})")]
    SchemaTooNew { found: usize, supported: usize },
    #[error("invalid value {value:?} stored in {column}")]
    InvalidValue { column: &'static str, value: String },
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Handle to the local database. All writes go through it.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens or creates the database file and applies pending migrations.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)?;
        migrations::apply(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn schema_version(&self) -> Result<usize> {
        migrations::current_version(&self.conn)
    }

    /// Registers a repository or updates its branch and GitHub name, and
    /// returns its id.
    pub fn upsert_repository(
        &self,
        path: &str,
        branch: Option<&str>,
        github: Option<&str>,
    ) -> Result<i64> {
        let id = self.conn.query_row(
            "INSERT INTO repositories (path, branch, github) VALUES (?1, ?2, ?3)
             ON CONFLICT (path) DO UPDATE SET branch = excluded.branch, github = excluded.github
             RETURNING id",
            (path, branch, github),
            |row| row.get(0),
        )?;
        Ok(id)
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_an_empty_file_and_reopens_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kawa.sqlite");

        let store = Store::open(&path).unwrap();
        let version = store.schema_version().unwrap();
        assert_eq!(
            version,
            migrations::MIGRATIONS.len(),
            "should apply every migration to a new file"
        );
        let id = store
            .upsert_repository("/src/app", Some("main"), None)
            .unwrap();
        drop(store);

        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.schema_version().unwrap(),
            version,
            "should not change the version when reopening"
        );
        assert_eq!(
            store
                .upsert_repository("/src/app", Some("main"), Some("acme/app"))
                .unwrap(),
            id,
            "should keep data and ids across reopen"
        );
    }

    #[test]
    fn creates_raw_and_derived_tables() {
        let store = Store::open_in_memory().unwrap();
        let mut statement = store
            .conn()
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
            .unwrap();
        let tables: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        for expected in [
            "repositories",
            "sync_status",
            "engine_snapshots",
            "identities",
            "commits",
            "commit_files",
            "line_removals",
            "pull_requests",
            "pr_commits",
            "pr_events",
            "reviews",
            "comments",
            "daily_developer_stats",
            "pr_timings",
        ] {
            assert!(
                tables.iter().any(|t| t == expected),
                "should create table {expected}, got {tables:?}"
            );
        }
    }

    #[test]
    fn rejects_a_database_from_a_newer_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kawa.sqlite");
        drop(Store::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 999).unwrap();
        drop(conn);

        assert!(
            matches!(
                Store::open(&path),
                Err(StoreError::SchemaTooNew { found: 999, .. })
            ),
            "should refuse to open a schema newer than the binary"
        );
    }

    #[test]
    fn enforces_foreign_keys() {
        let store = Store::open_in_memory().unwrap();
        let result = store
            .conn()
            .execute("INSERT INTO sync_status (repository_id) VALUES (42)", []);
        assert!(
            result.is_err(),
            "should reject rows pointing to missing repositories"
        );
    }
}
