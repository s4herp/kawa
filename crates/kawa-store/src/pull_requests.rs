//! Pull requests and their commits, reviews, comments and timeline events.

use kawa_github::{CommentKind, PullRequest};
use rusqlite::params;

use crate::{Result, Store};

impl Store {
    /// Replaces the stored copy of each pull request, children included, in
    /// one transaction.
    pub fn replace_pull_requests(
        &mut self,
        repository_id: i64,
        pull_requests: &[PullRequest],
    ) -> Result<()> {
        let tx = self.conn_mut().transaction()?;
        {
            let mut delete =
                tx.prepare("DELETE FROM pull_requests WHERE repository_id = ?1 AND number = ?2")?;
            let mut pr = tx.prepare(
                "INSERT INTO pull_requests (repository_id, number, node_id, author_login, title, url,
                   state, is_draft, created_at, updated_at, merged_at, closed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            let mut commit = tx.prepare(
                "INSERT OR IGNORE INTO pr_commits (repository_id, number, sha, author_login,
                   author_email, author_name, authored_at, committed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            let mut event = tx.prepare(
                "INSERT INTO pr_events (repository_id, number, seq, kind, actor_login, subject_login, at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut review = tx.prepare(
                "INSERT OR IGNORE INTO reviews (repository_id, number, node_id, author_login, state, submitted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            let mut comment = tx.prepare(
                "INSERT OR IGNORE INTO comments (repository_id, number, node_id, author_login, kind, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for p in pull_requests {
                delete.execute(params![repository_id, p.number])?;
                pr.execute(params![
                    repository_id,
                    p.number,
                    p.node_id,
                    p.author_login,
                    p.title,
                    p.url,
                    p.state.as_str(),
                    p.is_draft,
                    p.created_at.to_string(),
                    p.updated_at.to_string(),
                    p.merged_at.map(|t| t.to_string()),
                    p.closed_at.map(|t| t.to_string()),
                ])?;
                for c in &p.commits {
                    commit.execute(params![
                        repository_id,
                        p.number,
                        c.sha,
                        c.author_login,
                        c.author_email,
                        c.author_name,
                        c.authored_at.to_string(),
                        c.committed_at.to_string(),
                    ])?;
                }
                for (seq, e) in p.events.iter().enumerate() {
                    event.execute(params![
                        repository_id,
                        p.number,
                        seq as i64,
                        e.kind.as_str(),
                        e.actor_login,
                        e.subject_login,
                        e.at.to_string(),
                    ])?;
                }
                for r in &p.reviews {
                    review.execute(params![
                        repository_id,
                        p.number,
                        r.node_id,
                        r.author_login,
                        r.state,
                        r.submitted_at.to_string(),
                    ])?;
                }
                for c in &p.comments {
                    let kind = match c.kind {
                        CommentKind::Issue => "issue",
                        CommentKind::Review => "review",
                    };
                    comment.execute(params![
                        repository_id,
                        p.number,
                        c.node_id,
                        c.author_login,
                        kind,
                        c.created_at.to_string(),
                    ])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Counts of stored rows as `(pull_requests, commits, reviews, comments, events)`.
    pub fn pull_request_counts(&self, repository_id: i64) -> Result<(u64, u64, u64, u64, u64)> {
        let count = |table: &str| -> Result<u64> {
            Ok(self.conn().query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE repository_id = ?1"),
                [repository_id],
                |row| row.get(0),
            )?)
        };
        Ok((
            count("pull_requests")?,
            count("pr_commits")?,
            count("reviews")?,
            count("comments")?,
            count("pr_events")?,
        ))
    }

    /// Titles by PR number, to check what a sync stored.
    pub fn pull_request_titles(&self, repository_id: i64) -> Result<Vec<(i64, String)>> {
        let mut statement = self.conn().prepare(
            "SELECT number, title FROM pull_requests WHERE repository_id = ?1 ORDER BY number",
        )?;
        let rows = statement.query_map([repository_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}
