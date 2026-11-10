//! Commits, per-file line counts and line removal pairs from git replay.

use jiff::Timestamp;
use rusqlite::params;

use crate::{Result, Store};

/// Stored instead of a commit id for lines that predate the replay boundary.
pub const BASELINE: &str = "baseline";

#[derive(Debug, Clone)]
pub struct CommitInput {
    pub sha: String,
    pub author_name: String,
    pub author_email: String,
    pub author_at: Timestamp,
    pub author_offset_seconds: i32,
    pub subject: String,
    pub is_merge: bool,
    pub is_ignored: bool,
    pub files: Vec<FileInput>,
}

#[derive(Debug, Clone)]
pub struct FileInput {
    pub path: String,
    pub old_path: Option<String>,
    pub lines_added: u32,
    pub lines_removed: u32,
    pub new_work: u32,
}

#[derive(Debug, Clone)]
pub struct RemovalInput {
    /// `None` for baseline lines.
    pub introduced_sha: Option<String>,
    pub removed_sha: String,
    pub lines: u64,
}

impl Store {
    /// Replaces all git history stored for a repository in one transaction.
    pub fn replace_git_history(
        &mut self,
        repository_id: i64,
        commits: &[CommitInput],
        removals: &[RemovalInput],
    ) -> Result<()> {
        let tx = self.conn_mut().transaction()?;
        tx.execute(
            "DELETE FROM line_removals WHERE repository_id = ?1",
            [repository_id],
        )?;
        tx.execute(
            "DELETE FROM commit_files WHERE repository_id = ?1",
            [repository_id],
        )?;
        tx.execute(
            "DELETE FROM commits WHERE repository_id = ?1",
            [repository_id],
        )?;
        insert_history(&tx, repository_id, commits, removals)?;
        tx.commit()?;
        Ok(())
    }

    /// Adds commits and removal pairs from an incremental replay. Removal
    /// pairs for an existing (introduced, removed) key add their lines.
    pub fn append_git_history(
        &mut self,
        repository_id: i64,
        commits: &[CommitInput],
        removals: &[RemovalInput],
    ) -> Result<()> {
        let tx = self.conn_mut().transaction()?;
        insert_history(&tx, repository_id, commits, removals)?;
        tx.commit()?;
        Ok(())
    }

    pub fn commit_count(&self, repository_id: i64) -> Result<u64> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*) FROM commits WHERE repository_id = ?1",
            [repository_id],
            |row| row.get(0),
        )?)
    }

    /// Removal pairs as `(introduced, removed, lines)`, sorted.
    pub fn removal_pairs(&self, repository_id: i64) -> Result<Vec<(String, String, u64)>> {
        let mut statement = self.conn().prepare(
            "SELECT introduced_sha, removed_sha, lines FROM line_removals
             WHERE repository_id = ?1 ORDER BY introduced_sha, removed_sha",
        )?;
        let rows = statement.query_map([repository_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn insert_history(
    tx: &rusqlite::Transaction<'_>,
    repository_id: i64,
    commits: &[CommitInput],
    removals: &[RemovalInput],
) -> Result<()> {
    let mut identity = tx.prepare(
        "INSERT INTO identities (name, email, login) VALUES (?1, ?2, '')
         ON CONFLICT (name, email, login) DO UPDATE SET name = excluded.name
         RETURNING id",
    )?;
    let mut commit = tx.prepare(
        "INSERT INTO commits (repository_id, sha, author_identity_id, author_at,
                              author_offset_seconds, subject, is_merge, is_ignored)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    let mut file = tx.prepare(
        "INSERT INTO commit_files (repository_id, sha, path, old_path, lines_added,
                                   lines_removed, new_work)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for input in commits {
        let identity_id: i64 =
            identity.query_row((&input.author_name, &input.author_email), |row| row.get(0))?;
        commit.execute(params![
            repository_id,
            input.sha,
            identity_id,
            input.author_at.to_string(),
            input.author_offset_seconds,
            input.subject,
            input.is_merge,
            input.is_ignored,
        ])?;
        for f in &input.files {
            file.execute(params![
                repository_id,
                input.sha,
                f.path,
                f.old_path,
                f.lines_added,
                f.lines_removed,
                f.new_work,
            ])?;
        }
    }
    let mut removal = tx.prepare(
        "INSERT INTO line_removals (repository_id, introduced_sha, removed_sha, lines)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (repository_id, introduced_sha, removed_sha)
         DO UPDATE SET lines = lines + excluded.lines",
    )?;
    for input in removals {
        removal.execute(params![
            repository_id,
            input.introduced_sha.as_deref().unwrap_or(BASELINE),
            input.removed_sha,
            input.lines,
        ])?;
    }
    Ok(())
}
