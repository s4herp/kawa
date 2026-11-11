//! Per-repository record of the last successful syncs and the last error.

use jiff::Timestamp;
use rusqlite::{OptionalExtension, Row};

use crate::{Result, Store, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncErrorKind {
    Git,
    Github,
}

impl SyncErrorKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Github => "github",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "git" => Some(Self::Git),
            "github" => Some(Self::Github),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncStatus {
    pub git_synced_at: Option<Timestamp>,
    pub git_head: Option<String>,
    pub pr_synced_at: Option<Timestamp>,
    pub pr_high_water_mark: Option<Timestamp>,
    /// Oldest `updatedAt` the stored pull requests are complete from.
    pub pr_window_start: Option<Timestamp>,
    pub error: Option<SyncError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncError {
    pub kind: SyncErrorKind,
    pub message: String,
    pub at: Timestamp,
}

impl Store {
    /// Records a successful git sync and clears a previous git error.
    pub fn record_git_sync(&self, repository_id: i64, at: Timestamp, head: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO sync_status (repository_id, git_synced_at, git_head) VALUES (?1, ?2, ?3)
             ON CONFLICT (repository_id) DO UPDATE SET
               git_synced_at = excluded.git_synced_at,
               git_head = excluded.git_head,
               error_kind = CASE WHEN error_kind = 'git' THEN NULL ELSE error_kind END,
               error_message = CASE WHEN error_kind = 'git' THEN NULL ELSE error_message END,
               error_at = CASE WHEN error_kind = 'git' THEN NULL ELSE error_at END",
            (repository_id, at.to_string(), head),
        )?;
        Ok(())
    }

    /// Records a successful pull request sync that covers `window_start` up
    /// to `high_water_mark` (the latest `updatedAt` stored) and clears a
    /// previous GitHub error.
    pub fn record_pr_sync(
        &self,
        repository_id: i64,
        at: Timestamp,
        high_water_mark: Timestamp,
        window_start: Timestamp,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO sync_status (repository_id, pr_synced_at, pr_high_water_mark, pr_window_start)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (repository_id) DO UPDATE SET
               pr_synced_at = excluded.pr_synced_at,
               pr_high_water_mark = excluded.pr_high_water_mark,
               pr_window_start = excluded.pr_window_start,
               error_kind = CASE WHEN error_kind = 'github' THEN NULL ELSE error_kind END,
               error_message = CASE WHEN error_kind = 'github' THEN NULL ELSE error_message END,
               error_at = CASE WHEN error_kind = 'github' THEN NULL ELSE error_at END",
            (
                repository_id,
                at.to_string(),
                high_water_mark.to_string(),
                window_start.to_string(),
            ),
        )?;
        Ok(())
    }

    /// Records the latest sync failure, keeping the last successful sync times.
    pub fn record_sync_error(
        &self,
        repository_id: i64,
        kind: SyncErrorKind,
        at: Timestamp,
        message: &str,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO sync_status (repository_id, error_kind, error_message, error_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (repository_id) DO UPDATE SET
               error_kind = excluded.error_kind,
               error_message = excluded.error_message,
               error_at = excluded.error_at",
            (repository_id, kind.as_str(), message, at.to_string()),
        )?;
        Ok(())
    }

    /// Returns the status of a repository; a repository never synced has an
    /// empty status.
    pub fn sync_status(&self, repository_id: i64) -> Result<SyncStatus> {
        let row = self
            .conn()
            .query_row(
                "SELECT git_synced_at, git_head, pr_synced_at, pr_high_water_mark,
                        error_kind, error_message, error_at, pr_window_start
                 FROM sync_status WHERE repository_id = ?1",
                [repository_id],
                RawStatus::from_row,
            )
            .optional()?;
        row.map_or_else(|| Ok(SyncStatus::default()), RawStatus::into_status)
    }
}

struct RawStatus {
    git_synced_at: Option<String>,
    git_head: Option<String>,
    pr_synced_at: Option<String>,
    pr_high_water_mark: Option<String>,
    pr_window_start: Option<String>,
    error_kind: Option<String>,
    error_message: Option<String>,
    error_at: Option<String>,
}

impl RawStatus {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            git_synced_at: row.get(0)?,
            git_head: row.get(1)?,
            pr_synced_at: row.get(2)?,
            pr_high_water_mark: row.get(3)?,
            error_kind: row.get(4)?,
            error_message: row.get(5)?,
            error_at: row.get(6)?,
            pr_window_start: row.get(7)?,
        })
    }

    fn into_status(self) -> Result<SyncStatus> {
        let error = match (self.error_kind, self.error_message, self.error_at) {
            (Some(kind), Some(message), Some(at)) => Some(SyncError {
                kind: SyncErrorKind::parse(&kind).ok_or(StoreError::InvalidValue {
                    column: "error_kind",
                    value: kind.clone(),
                })?,
                message,
                at: parse_timestamp("error_at", at)?,
            }),
            _ => None,
        };
        Ok(SyncStatus {
            git_synced_at: self
                .git_synced_at
                .map(|v| parse_timestamp("git_synced_at", v))
                .transpose()?,
            git_head: self.git_head,
            pr_synced_at: self
                .pr_synced_at
                .map(|v| parse_timestamp("pr_synced_at", v))
                .transpose()?,
            pr_high_water_mark: self
                .pr_high_water_mark
                .map(|v| parse_timestamp("pr_high_water_mark", v))
                .transpose()?,
            pr_window_start: self
                .pr_window_start
                .map(|v| parse_timestamp("pr_window_start", v))
                .transpose()?,
            error,
        })
    }
}

fn parse_timestamp(column: &'static str, value: String) -> Result<Timestamp> {
    value
        .parse()
        .map_err(|_| StoreError::InvalidValue { column, value })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(value: &str) -> Timestamp {
        value.parse().unwrap()
    }

    fn store_with_repo() -> (Store, i64) {
        let store = Store::open_in_memory().unwrap();
        let id = store
            .upsert_repository("/src/app", None, Some("acme/app"))
            .unwrap();
        (store, id)
    }

    #[test]
    fn never_synced_repository_has_empty_status() {
        let (store, id) = store_with_repo();
        assert_eq!(
            store.sync_status(id).unwrap(),
            SyncStatus::default(),
            "should report no syncs and no error before the first sync"
        );
    }

    #[test]
    fn records_git_and_pr_syncs_independently() {
        let (store, id) = store_with_repo();
        store
            .record_git_sync(id, ts("2026-11-10T12:00:00Z"), "abc123")
            .unwrap();
        store
            .record_pr_sync(
                id,
                ts("2026-11-10T12:05:00Z"),
                ts("2026-11-10T11:59:00Z"),
                ts("2026-08-12T12:05:00Z"),
            )
            .unwrap();

        let status = store.sync_status(id).unwrap();
        assert_eq!(
            status.git_synced_at,
            Some(ts("2026-11-10T12:00:00Z")),
            "should keep the git sync time"
        );
        assert_eq!(
            status.git_head.as_deref(),
            Some("abc123"),
            "should keep the synced head"
        );
        assert_eq!(
            status.pr_synced_at,
            Some(ts("2026-11-10T12:05:00Z")),
            "should keep the PR sync time"
        );
        assert_eq!(
            status.pr_high_water_mark,
            Some(ts("2026-11-10T11:59:00Z")),
            "should keep the PR high-water mark"
        );
        assert_eq!(
            status.error, None,
            "should have no error after successful syncs"
        );
    }

    #[test]
    fn error_keeps_last_success_and_is_cleared_by_same_kind_only() {
        let (store, id) = store_with_repo();
        store
            .record_git_sync(id, ts("2026-11-10T12:00:00Z"), "abc123")
            .unwrap();
        store
            .record_sync_error(
                id,
                SyncErrorKind::Github,
                ts("2026-11-10T12:10:00Z"),
                "rate limited",
            )
            .unwrap();

        let status = store.sync_status(id).unwrap();
        assert_eq!(
            status.git_synced_at,
            Some(ts("2026-11-10T12:00:00Z")),
            "should keep the last successful git sync after a GitHub failure"
        );
        assert_eq!(
            status.error,
            Some(SyncError {
                kind: SyncErrorKind::Github,
                message: "rate limited".to_owned(),
                at: ts("2026-11-10T12:10:00Z"),
            }),
            "should record the failure with kind, message and time"
        );

        store
            .record_git_sync(id, ts("2026-11-10T13:00:00Z"), "def456")
            .unwrap();
        assert!(
            store.sync_status(id).unwrap().error.is_some(),
            "should keep a GitHub error when only git sync succeeds"
        );

        store
            .record_pr_sync(
                id,
                ts("2026-11-10T13:05:00Z"),
                ts("2026-11-10T13:00:00Z"),
                ts("2026-08-12T12:05:00Z"),
            )
            .unwrap();
        assert_eq!(
            store.sync_status(id).unwrap().error,
            None,
            "should clear the GitHub error after a successful PR sync"
        );
    }
}
