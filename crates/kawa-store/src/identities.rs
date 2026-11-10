//! Raw identities, their resolution to developers, and the identity audit.

use rusqlite::params;

use crate::{Result, Store};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityRow {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub login: String,
    /// Commits authored by this identity; zero for logins.
    pub commits: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityResolution {
    pub identity_id: i64,
    pub developer_id: Option<String>,
    pub excluded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginEvidenceRow {
    pub login: String,
    pub name: String,
    pub email: String,
    pub commits: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRow {
    pub name: String,
    pub email: String,
    pub login: String,
    pub developer_id: Option<String>,
    pub developer_name: Option<String>,
    pub excluded: bool,
    pub commits: u64,
    pub pull_requests: u64,
    pub reviews: u64,
}

impl Store {
    pub fn upsert_git_identity(&self, name: &str, email: &str) -> Result<i64> {
        self.upsert_identity(name, email, "")
    }

    pub fn upsert_login_identity(&self, login: &str) -> Result<i64> {
        self.upsert_identity("", "", login)
    }

    fn upsert_identity(&self, name: &str, email: &str, login: &str) -> Result<i64> {
        let id = self.conn().query_row(
            "INSERT INTO identities (name, email, login) VALUES (?1, ?2, ?3)
             ON CONFLICT (name, email, login) DO UPDATE SET name = excluded.name
             RETURNING id",
            (name, email, login),
            |row| row.get(0),
        )?;
        Ok(id)
    }

    /// Registers every login that appears in pull request data as a login identity.
    pub fn register_pr_logins(&self) -> Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO identities (name, email, login)
             SELECT '', '', login FROM (
               SELECT author_login AS login FROM pull_requests
               UNION SELECT author_login FROM reviews
               UNION SELECT author_login FROM comments
               UNION SELECT author_login FROM pr_commits WHERE author_login IS NOT NULL
               UNION SELECT actor_login FROM pr_events WHERE actor_login IS NOT NULL
               UNION SELECT subject_login FROM pr_events WHERE subject_login IS NOT NULL
             ) WHERE login <> ''",
            [],
        )?;
        Ok(())
    }

    pub fn identities(&self) -> Result<Vec<IdentityRow>> {
        let mut statement = self.conn().prepare(
            "SELECT i.id, i.name, i.email, i.login, COUNT(c.sha)
             FROM identities i LEFT JOIN commits c ON c.author_identity_id = i.id
             GROUP BY i.id ORDER BY i.id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(IdentityRow {
                id: row.get(0)?,
                name: row.get(1)?,
                email: row.get(2)?,
                login: row.get(3)?,
                commits: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Git identities GitHub reports on each login's PR commits, with counts.
    pub fn login_evidence(&self) -> Result<Vec<LoginEvidenceRow>> {
        let mut statement = self.conn().prepare(
            "SELECT author_login, author_name, author_email, COUNT(*)
             FROM pr_commits WHERE author_login IS NOT NULL AND author_login <> ''
             GROUP BY author_login, author_name, author_email
             ORDER BY author_login, author_name, author_email",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(LoginEvidenceRow {
                login: row.get(0)?,
                name: row.get(1)?,
                email: row.get(2)?,
                commits: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replaces every identity's resolution and the developer list in one transaction.
    pub fn save_resolutions(
        &mut self,
        resolutions: &[IdentityResolution],
        developers: &[(String, String)],
    ) -> Result<()> {
        let tx = self.conn_mut().transaction()?;
        {
            let mut update =
                tx.prepare("UPDATE identities SET developer_id = ?2, excluded = ?3 WHERE id = ?1")?;
            for resolution in resolutions {
                update.execute(params![
                    resolution.identity_id,
                    resolution.developer_id,
                    resolution.excluded
                ])?;
            }
            tx.execute("DELETE FROM developers", [])?;
            let mut insert =
                tx.prepare("INSERT INTO developers (id, display_name) VALUES (?1, ?2)")?;
            for (id, name) in developers {
                insert.execute((id, name))?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every raw identity with its resolution and activity counts, ordered by
    /// developer name, then identity.
    pub fn identity_audit(&self) -> Result<Vec<AuditRow>> {
        let mut statement = self.conn().prepare(
            "SELECT i.name, i.email, i.login, i.developer_id, d.display_name, i.excluded,
                    (SELECT COUNT(*) FROM commits c WHERE c.author_identity_id = i.id),
                    CASE WHEN i.login = '' THEN 0 ELSE
                      (SELECT COUNT(*) FROM pull_requests p WHERE p.author_login = i.login) END,
                    CASE WHEN i.login = '' THEN 0 ELSE
                      (SELECT COUNT(*) FROM reviews r WHERE r.author_login = i.login) END
             FROM identities i LEFT JOIN developers d ON d.id = i.developer_id
             ORDER BY i.excluded, COALESCE(d.display_name, '') COLLATE NOCASE,
                      i.login, i.email COLLATE NOCASE, i.name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(AuditRow {
                name: row.get(0)?,
                email: row.get(1)?,
                login: row.get(2)?,
                developer_id: row.get(3)?,
                developer_name: row.get(4)?,
                excluded: row.get(5)?,
                commits: row.get(6)?,
                pull_requests: row.get(7)?,
                reviews: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_returns_the_same_id_for_the_same_identity() {
        let store = Store::open_in_memory().unwrap();
        let first = store.upsert_git_identity("Ana", "ana@x.example").unwrap();
        let again = store.upsert_git_identity("Ana", "ana@x.example").unwrap();
        let login = store.upsert_login_identity("ana-gh").unwrap();
        assert_eq!(first, again, "should not duplicate an identity seen twice");
        assert_ne!(first, login, "should keep git and login identities apart");
    }

    #[test]
    fn registers_logins_found_in_pr_data() {
        let store = Store::open_in_memory().unwrap();
        store
            .execute_batch(
                "INSERT INTO repositories (id, path) VALUES (1, '/r');
                 INSERT INTO pull_requests VALUES (1, 7, 'PR_1', 'ana-gh', 't', 'u', 'merged', 0,
                   '2026-01-01T00:00:00Z', '2026-01-02T00:00:00Z', '2026-01-02T00:00:00Z', NULL);
                 INSERT INTO reviews VALUES (1, 7, 'R_1', 'luis-gh', 'APPROVED', '2026-01-01T10:00:00Z');",
            )
            .unwrap();
        store.register_pr_logins().unwrap();
        store.register_pr_logins().unwrap();
        let logins: Vec<String> = store
            .identities()
            .unwrap()
            .into_iter()
            .map(|i| i.login)
            .collect();
        assert_eq!(
            logins,
            vec!["ana-gh".to_owned(), "luis-gh".to_owned()],
            "should register each PR author and reviewer login once"
        );
    }
}
