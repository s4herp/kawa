//! Per-developer, per-day activity: commits, pull requests, reviews and comments.

use std::collections::{BTreeMap, HashMap};

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use rusqlite::params;

use crate::fundamentals::{Selection, WorkTypes};
use crate::{Result, Store, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemKind {
    Commit {
        sha: String,
        lines_added: u64,
        lines_removed: u64,
        work: WorkTypes,
    },
    PullRequestOpened {
        number: i64,
    },
    PullRequestMerged {
        number: i64,
    },
    Review {
        number: i64,
        state: String,
    },
    Comment {
        number: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkLogItem {
    pub developer_id: String,
    pub day: String,
    pub at: Timestamp,
    pub repository_id: i64,
    /// Commit subject or pull request title.
    pub title: String,
    /// GitHub link when the repository has a GitHub name.
    pub url: Option<String>,
    pub kind: ItemKind,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DayActivity {
    pub commits: u64,
    pub prs_opened: u64,
    pub prs_merged: u64,
    pub reviews: u64,
    pub comments: u64,
}

impl DayActivity {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn count(&mut self, kind: &ItemKind) {
        match kind {
            ItemKind::Commit { .. } => self.commits += 1,
            ItemKind::PullRequestOpened { .. } => self.prs_opened += 1,
            ItemKind::PullRequestMerged { .. } => self.prs_merged += 1,
            ItemKind::Review { .. } => self.reviews += 1,
            ItemKind::Comment { .. } => self.comments += 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryRow {
    pub id: i64,
    pub path: String,
    pub github: Option<String>,
}

impl Store {
    pub fn repositories(&self) -> Result<Vec<RepositoryRow>> {
        let mut statement = self
            .conn()
            .prepare("SELECT id, path, github FROM repositories ORDER BY path")?;
        let rows = statement.query_map([], |r| {
            Ok(RepositoryRow {
                id: r.get(0)?,
                path: r.get(1)?,
                github: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every work-log item in the selection, oldest first. Merge commits and
    /// items by excluded identities are left out.
    pub fn work_log_items(
        &self,
        selection: &Selection,
        timezone: &TimeZone,
    ) -> Result<Vec<WorkLogItem>> {
        let (start, end) = (selection.start.to_string(), selection.end.to_string());
        let github: HashMap<i64, String> = self
            .repositories()?
            .into_iter()
            .filter_map(|r| r.github.map(|g| (r.id, g)))
            .collect();
        let logins = self.login_developers()?;
        let mut items = Vec::new();

        let mut commits = self.conn().prepare(
            "SELECT s.repository_id, s.sha, s.developer_id, s.day, c.author_at, c.subject,
                    s.lines_added, s.lines_removed, s.new_work, s.legacy_refactor, s.help_others, s.churn
             FROM commit_stats s JOIN commits c ON c.repository_id = s.repository_id AND c.sha = s.sha
             WHERE s.is_merge = 0 AND s.day BETWEEN ?1 AND ?2",
        )?;
        for row in commits.query_map(params![start, end], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                [r.get::<_, u64>(6)?, r.get::<_, u64>(7)?],
                WorkTypes {
                    new_work: r.get(8)?,
                    legacy_refactor: r.get(9)?,
                    help_others: r.get(10)?,
                    churn: r.get(11)?,
                },
            ))
        })? {
            let (repository_id, sha, developer_id, day, at, subject, [added, removed], work) = row?;
            items.push(WorkLogItem {
                url: github
                    .get(&repository_id)
                    .map(|g| format!("https://github.com/{g}/commit/{sha}")),
                developer_id,
                day,
                at: parse(at)?,
                repository_id,
                title: subject,
                kind: ItemKind::Commit {
                    sha,
                    lines_added: added,
                    lines_removed: removed,
                    work,
                },
            });
        }

        let mut prs = self.conn().prepare(
            "SELECT repository_id, number, author_login, title, url, created_at, merged_at FROM pull_requests",
        )?;
        for row in prs.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })? {
            let (repository_id, number, login, title, url, created, merged) = row?;
            let Some(developer_id) = resolve(&logins, &login) else {
                continue;
            };
            let mut events = vec![(parse(created)?, ItemKind::PullRequestOpened { number })];
            if let Some(merged) = merged {
                events.push((parse(merged)?, ItemKind::PullRequestMerged { number }));
            }
            for (at, kind) in events {
                items.push(WorkLogItem {
                    developer_id: developer_id.clone(),
                    day: day_of(at, timezone),
                    at,
                    repository_id,
                    title: title.clone(),
                    url: Some(url.clone()),
                    kind,
                });
            }
        }

        for (sql, is_review) in [
            (
                "SELECT r.repository_id, r.number, r.author_login, r.submitted_at, p.title, p.url, r.state
                 FROM reviews r JOIN pull_requests p ON p.repository_id = r.repository_id AND p.number = r.number",
                true,
            ),
            (
                "SELECT c.repository_id, c.number, c.author_login, c.created_at, p.title, p.url, NULL
                 FROM comments c JOIN pull_requests p ON p.repository_id = c.repository_id AND p.number = c.number",
                false,
            ),
        ] {
            let mut statement = self.conn().prepare(sql)?;
            for row in statement.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            })? {
                let (repository_id, number, login, at, title, url, state) = row?;
                let Some(developer_id) = resolve(&logins, &login) else {
                    continue;
                };
                let at = parse(at)?;
                items.push(WorkLogItem {
                    developer_id,
                    day: day_of(at, timezone),
                    at,
                    repository_id,
                    title,
                    url: Some(url),
                    kind: if is_review {
                        ItemKind::Review {
                            number,
                            state: state.unwrap_or_default(),
                        }
                    } else {
                        ItemKind::Comment { number }
                    },
                });
            }
        }

        items.retain(|item| {
            item.day.as_str() >= start.as_str()
                && item.day.as_str() <= end.as_str()
                && (selection.repositories.is_empty()
                    || selection.repositories.contains(&item.repository_id))
                && (selection.developers.is_empty()
                    || selection.developers.contains(&item.developer_id))
        });
        items.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.title.cmp(&b.title)));
        Ok(items)
    }

    /// Counts per developer and day, derived from the same items as the drill-down.
    pub fn work_log(
        &self,
        selection: &Selection,
        timezone: &TimeZone,
    ) -> Result<BTreeMap<String, BTreeMap<String, DayActivity>>> {
        let mut grid: BTreeMap<String, BTreeMap<String, DayActivity>> = BTreeMap::new();
        for item in self.work_log_items(selection, timezone)? {
            grid.entry(item.developer_id)
                .or_default()
                .entry(item.day)
                .or_default()
                .count(&item.kind);
        }
        Ok(grid)
    }

    /// Work types of a developer per week (weeks start on Monday).
    pub fn weekly_work(
        &self,
        selection: &Selection,
        developer_id: &str,
    ) -> Result<Vec<(Date, WorkTypes)>> {
        let mut statement = self.conn().prepare(
            "SELECT repository_id, day, new_work, legacy_refactor, help_others, churn
             FROM commit_stats WHERE developer_id = ?1 AND day BETWEEN ?2 AND ?3",
        )?;
        let mut weeks: BTreeMap<Date, WorkTypes> = BTreeMap::new();
        for row in statement.query_map(
            params![
                developer_id,
                selection.start.to_string(),
                selection.end.to_string()
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    WorkTypes {
                        new_work: r.get(2)?,
                        legacy_refactor: r.get(3)?,
                        help_others: r.get(4)?,
                        churn: r.get(5)?,
                    },
                ))
            },
        )? {
            let (repository_id, day, work) = row?;
            if !selection.repositories.is_empty()
                && !selection.repositories.contains(&repository_id)
            {
                continue;
            }
            let date: Date = day.parse().map_err(|_| StoreError::InvalidValue {
                column: "commit_stats.day",
                value: day.clone(),
            })?;
            let back = i64::from(date.weekday().to_monday_zero_offset());
            let monday = date
                .checked_sub(jiff::Span::new().days(back))
                .unwrap_or(date);
            let entry = weeks.entry(monday).or_default();
            entry.new_work += work.new_work;
            entry.legacy_refactor += work.legacy_refactor;
            entry.help_others += work.help_others;
            entry.churn += work.churn;
        }
        Ok(weeks.into_iter().collect())
    }
}

fn resolve(logins: &HashMap<String, Option<String>>, login: &str) -> Option<String> {
    match logins.get(login) {
        Some(resolved) => resolved.clone(),
        None => Some(format!("gh:{}", login.to_lowercase())),
    }
}

fn parse(value: String) -> Result<Timestamp> {
    value.parse().map_err(|_| StoreError::InvalidValue {
        column: "timestamp",
        value,
    })
}

fn day_of(at: Timestamp, timezone: &TimeZone) -> String {
    at.to_zoned(timezone.clone()).date().to_string()
}
