//! Code fundamentals: per-commit work types, per-developer aggregates and the
//! commits behind each value.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use rusqlite::params;

use crate::git_history::BASELINE;
use crate::{Result, Store, StoreError};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkTypes {
    pub new_work: u64,
    pub legacy_refactor: u64,
    pub help_others: u64,
    pub churn: u64,
}

impl WorkTypes {
    pub fn changed(&self) -> u64 {
        self.new_work + self.legacy_refactor + self.help_others + self.churn
    }

    /// Shares of new work, legacy refactor, helping others and churn, summing to 1.0.
    pub fn shares(&self) -> Option<[f64; 4]> {
        let changed = self.changed();
        (changed > 0).then(|| {
            [
                self.new_work,
                self.legacy_refactor,
                self.help_others,
                self.churn,
            ]
            .map(|n| n as f64 / changed as f64)
        })
    }

    /// Share of changed lines that are not churn.
    pub fn efficiency(&self) -> Option<f64> {
        let changed = self.changed();
        (changed > 0).then(|| 1.0 - self.churn as f64 / changed as f64)
    }

    fn add(&mut self, other: &WorkTypes) {
        self.new_work += other.new_work;
        self.legacy_refactor += other.legacy_refactor;
        self.help_others += other.help_others;
        self.churn += other.churn;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkType {
    NewWork,
    LegacyRefactor,
    HelpOthers,
    Churn,
}

#[derive(Debug, Clone, Copy)]
pub struct ImpactSettings {
    pub lines: f64,
    pub files: f64,
    pub modules: f64,
    pub module_depth: usize,
    pub min_lines: u64,
}

/// Selection shared by every fundamentals query. Empty sets mean "all".
#[derive(Debug, Clone)]
pub struct Selection {
    pub start: Date,
    pub end: Date,
    pub repositories: BTreeSet<i64>,
    pub developers: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeveloperFundamentals {
    pub developer_id: String,
    pub name: String,
    pub active_days: u64,
    pub commits: u64,
    pub commits_per_active_day: f64,
    pub work: WorkTypes,
    pub lines_changed: u64,
    pub files: u64,
    pub modules: u64,
    /// `None` when the developer has fewer changed lines than `min_lines`.
    pub efficiency: Option<f64>,
    pub impact: Option<f64>,
}

/// One commit behind a fundamentals value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitEvidence {
    pub repository_id: i64,
    pub sha: String,
    pub day: String,
    pub subject: String,
    pub work: WorkTypes,
    /// Earlier commits whose lines this commit removed, with the line count
    /// and how each was classified.
    pub rewrote: Vec<RewroteLines>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewroteLines {
    /// `None` for lines older than the replay boundary.
    pub introduced_sha: Option<String>,
    pub introduced_by: Option<String>,
    pub lines: u64,
    pub work_type: WorkType,
}

struct CommitRow {
    developer_id: Option<String>,
    at: Timestamp,
    is_merge: bool,
    is_ignored: bool,
}

impl Store {
    /// Rebuilds `commit_stats` from raw history with the current identities,
    /// timezone and recency threshold.
    pub fn rebuild_commit_stats(&mut self, timezone: &TimeZone, recency_days: u32) -> Result<()> {
        let commits = self.load_commit_rows()?;
        let mut work: HashMap<(i64, String), WorkTypes> = HashMap::new();
        {
            let mut statement = self.conn().prepare(
                "SELECT repository_id, sha, SUM(new_work) FROM commit_files GROUP BY repository_id, sha",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u64>(2)?,
                ))
            })?;
            for row in rows {
                let (repository_id, sha, new_work) = row?;
                work.entry((repository_id, sha)).or_default().new_work = new_work;
            }
        }
        for (repository_id, introduced, removed, lines) in self.all_removals()? {
            let Some(remover) = commits.get(&(repository_id, removed.clone())) else {
                continue;
            };
            let work_type = classify(&commits, repository_id, &introduced, remover, recency_days);
            let entry = work.entry((repository_id, removed)).or_default();
            match work_type {
                WorkType::LegacyRefactor => entry.legacy_refactor += lines,
                WorkType::HelpOthers => entry.help_others += lines,
                WorkType::Churn => entry.churn += lines,
                WorkType::NewWork => entry.new_work += lines,
            }
        }
        let mut added_removed: HashMap<(i64, String), (u64, u64)> = HashMap::new();
        {
            let mut statement = self.conn().prepare(
                "SELECT repository_id, sha, SUM(lines_added), SUM(lines_removed)
                 FROM commit_files GROUP BY repository_id, sha",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, u64>(3)?,
                ))
            })?;
            for row in rows {
                let (repository_id, sha, added, removed) = row?;
                added_removed.insert((repository_id, sha), (added, removed));
            }
        }

        let tx = self.conn_mut().transaction()?;
        tx.execute("DELETE FROM commit_stats", [])?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO commit_stats (repository_id, sha, developer_id, day, is_merge,
                   lines_added, lines_removed, new_work, legacy_refactor, help_others, churn)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            for ((repository_id, sha), commit) in &commits {
                let Some(developer_id) = &commit.developer_id else {
                    continue;
                };
                if commit.is_ignored {
                    continue;
                }
                let key = (*repository_id, sha.clone());
                let w = work.get(&key).copied().unwrap_or_default();
                let (added, removed) = added_removed.get(&key).copied().unwrap_or_default();
                let day = commit.at.to_zoned(timezone.clone()).date().to_string();
                insert.execute(params![
                    repository_id,
                    sha,
                    developer_id,
                    day,
                    commit.is_merge,
                    added,
                    removed,
                    w.new_work,
                    w.legacy_refactor,
                    w.help_others,
                    w.churn,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn load_commit_rows(&self) -> Result<HashMap<(i64, String), CommitRow>> {
        let mut statement = self.conn().prepare(
            "SELECT c.repository_id, c.sha, i.developer_id, i.excluded, c.author_at, c.is_merge, c.is_ignored
             FROM commits c JOIN identities i ON i.id = c.author_identity_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, bool>(5)?,
                row.get::<_, bool>(6)?,
            ))
        })?;
        let mut commits = HashMap::new();
        for row in rows {
            let (repository_id, sha, developer_id, excluded, at, is_merge, is_ignored) = row?;
            let at: Timestamp = at.parse().map_err(|_| StoreError::InvalidValue {
                column: "commits.author_at",
                value: at.clone(),
            })?;
            commits.insert(
                (repository_id, sha),
                CommitRow {
                    developer_id: if excluded { None } else { developer_id },
                    at,
                    is_merge,
                    is_ignored,
                },
            );
        }
        Ok(commits)
    }

    fn all_removals(&self) -> Result<Vec<(i64, String, String, u64)>> {
        let mut statement = self.conn().prepare(
            "SELECT repository_id, introduced_sha, removed_sha, lines FROM line_removals",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Fundamentals per developer with activity in the selection, sorted by
    /// display name.
    pub fn fundamentals(
        &self,
        selection: &Selection,
        impact: &ImpactSettings,
    ) -> Result<Vec<DeveloperFundamentals>> {
        struct Acc {
            days: BTreeSet<String>,
            commits: u64,
            work: WorkTypes,
            lines: u64,
            files: BTreeSet<(i64, String)>,
            modules: BTreeSet<(i64, String)>,
        }
        let mut by_developer: BTreeMap<String, Acc> = BTreeMap::new();
        {
            let mut statement = self.conn().prepare(
                "SELECT repository_id, developer_id, day, is_merge, lines_added, lines_removed,
                        new_work, legacy_refactor, help_others, churn
                 FROM commit_stats WHERE day BETWEEN ?1 AND ?2",
            )?;
            let rows = statement.query_map(
                params![selection.start.to_string(), selection.end.to_string()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, bool>(3)?,
                        row.get::<_, u64>(4)? + row.get::<_, u64>(5)?,
                        WorkTypes {
                            new_work: row.get(6)?,
                            legacy_refactor: row.get(7)?,
                            help_others: row.get(8)?,
                            churn: row.get(9)?,
                        },
                    ))
                },
            )?;
            for row in rows {
                let (repository_id, developer_id, day, is_merge, lines, work) = row?;
                if !selection.includes(repository_id, &developer_id) {
                    continue;
                }
                let acc = by_developer.entry(developer_id).or_insert_with(|| Acc {
                    days: BTreeSet::new(),
                    commits: 0,
                    work: WorkTypes::default(),
                    lines: 0,
                    files: BTreeSet::new(),
                    modules: BTreeSet::new(),
                });
                if !is_merge {
                    acc.days.insert(day);
                    acc.commits += 1;
                }
                acc.work.add(&work);
                acc.lines += lines;
            }
        }
        {
            let mut statement = self.conn().prepare(
                "SELECT s.repository_id, s.developer_id, f.path FROM commit_stats s
                 JOIN commit_files f ON f.repository_id = s.repository_id AND f.sha = s.sha
                 WHERE s.day BETWEEN ?1 AND ?2",
            )?;
            let rows = statement.query_map(
                params![selection.start.to_string(), selection.end.to_string()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?;
            for row in rows {
                let (repository_id, developer_id, path) = row?;
                if let Some(acc) = by_developer.get_mut(&developer_id)
                    && selection.includes(repository_id, &developer_id)
                {
                    acc.modules
                        .insert((repository_id, module_of(&path, impact.module_depth)));
                    acc.files.insert((repository_id, path));
                }
            }
        }

        let names = self.developer_names()?;
        let lines: Vec<f64> = by_developer.values().map(|a| a.lines as f64).collect();
        let files: Vec<f64> = by_developer
            .values()
            .map(|a| a.files.len() as f64)
            .collect();
        let modules: Vec<f64> = by_developer
            .values()
            .map(|a| a.modules.len() as f64)
            .collect();
        let mut result: Vec<DeveloperFundamentals> = by_developer
            .into_iter()
            .map(|(developer_id, acc)| {
                let changed = acc.work.changed();
                let eligible = changed >= impact.min_lines;
                let active_days = acc.days.len() as u64;
                DeveloperFundamentals {
                    name: names
                        .get(&developer_id)
                        .cloned()
                        .unwrap_or_else(|| developer_id.clone()),
                    developer_id,
                    active_days,
                    commits: acc.commits,
                    commits_per_active_day: if active_days == 0 {
                        0.0
                    } else {
                        acc.commits as f64 / active_days as f64
                    },
                    work: acc.work,
                    lines_changed: acc.lines,
                    files: acc.files.len() as u64,
                    modules: acc.modules.len() as u64,
                    efficiency: acc.work.efficiency().filter(|_| eligible),
                    impact: eligible.then(|| {
                        impact.lines * percentile_rank(&lines, acc.lines as f64)
                            + impact.files * percentile_rank(&files, acc.files.len() as f64)
                            + impact.modules * percentile_rank(&modules, acc.modules.len() as f64)
                    }),
                }
            })
            .collect();
        result.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.developer_id.cmp(&b.developer_id))
        });
        Ok(result)
    }

    pub fn developer_names(&self) -> Result<HashMap<String, String>> {
        let mut statement = self
            .conn()
            .prepare("SELECT id, display_name FROM developers")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The commits of `developer_id` in the selection with their work types
    /// and the earlier commits each one rewrote, newest first.
    pub fn fundamentals_evidence(
        &self,
        selection: &Selection,
        developer_id: &str,
        recency_days: u32,
    ) -> Result<Vec<CommitEvidence>> {
        let mut statement = self.conn().prepare(
            "SELECT s.repository_id, s.sha, s.day, c.subject,
                    s.new_work, s.legacy_refactor, s.help_others, s.churn
             FROM commit_stats s JOIN commits c ON c.repository_id = s.repository_id AND c.sha = s.sha
             WHERE s.developer_id = ?1 AND s.day BETWEEN ?2 AND ?3
             ORDER BY c.author_at DESC, s.sha",
        )?;
        let rows = statement.query_map(
            params![
                developer_id,
                selection.start.to_string(),
                selection.end.to_string()
            ],
            |row| {
                Ok(CommitEvidence {
                    repository_id: row.get(0)?,
                    sha: row.get(1)?,
                    day: row.get(2)?,
                    subject: row.get(3)?,
                    work: WorkTypes {
                        new_work: row.get(4)?,
                        legacy_refactor: row.get(5)?,
                        help_others: row.get(6)?,
                        churn: row.get(7)?,
                    },
                    rewrote: Vec::new(),
                })
            },
        )?;
        let mut evidence: Vec<CommitEvidence> = rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter(|e| selection.includes(e.repository_id, developer_id))
            .collect();
        if evidence.is_empty() {
            return Ok(evidence);
        }
        let commits = self.load_commit_rows()?;
        let mut removals = self.conn().prepare(
            "SELECT introduced_sha, lines FROM line_removals
             WHERE repository_id = ?1 AND removed_sha = ?2 ORDER BY introduced_sha",
        )?;
        for item in &mut evidence {
            let Some(remover) = commits.get(&(item.repository_id, item.sha.clone())) else {
                continue;
            };
            let rows = removals.query_map(params![item.repository_id, item.sha], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?;
            for row in rows {
                let (introduced, lines) = row?;
                let work_type = classify(
                    &commits,
                    item.repository_id,
                    &introduced,
                    remover,
                    recency_days,
                );
                let baseline = introduced == BASELINE;
                item.rewrote.push(RewroteLines {
                    introduced_by: if baseline {
                        None
                    } else {
                        commits
                            .get(&(item.repository_id, introduced.clone()))
                            .and_then(|c| c.developer_id.clone())
                    },
                    introduced_sha: (!baseline).then_some(introduced),
                    lines,
                    work_type,
                });
            }
        }
        Ok(evidence)
    }
}

impl Selection {
    fn includes(&self, repository_id: i64, developer_id: &str) -> bool {
        (self.repositories.is_empty() || self.repositories.contains(&repository_id))
            && (self.developers.is_empty() || self.developers.contains(developer_id))
    }
}

/// Removed lines are churn when their own author removes them within the
/// recency window, helping others when someone else does, and legacy refactor
/// when older, from before the boundary, or introduced by an excluded identity.
fn classify(
    commits: &HashMap<(i64, String), CommitRow>,
    repository_id: i64,
    introduced: &str,
    remover: &CommitRow,
    recency_days: u32,
) -> WorkType {
    if introduced == BASELINE {
        return WorkType::LegacyRefactor;
    }
    let Some(origin) = commits.get(&(repository_id, introduced.to_owned())) else {
        return WorkType::LegacyRefactor;
    };
    let Some(origin_developer) = &origin.developer_id else {
        return WorkType::LegacyRefactor;
    };
    let age = remover.at.as_second() - origin.at.as_second();
    if age >= i64::from(recency_days) * 86_400 {
        return WorkType::LegacyRefactor;
    }
    if remover.developer_id.as_ref() == Some(origin_developer) {
        WorkType::Churn
    } else {
        WorkType::HelpOthers
    }
}

/// Directory prefix of `path` with at most `depth` segments; files at the
/// repository root belong to module `.`.
pub fn module_of(path: &str, depth: usize) -> String {
    let directories: Vec<&str> = path.split('/').collect();
    let directories = &directories[..directories.len().saturating_sub(1)];
    if directories.is_empty() {
        return ".".to_owned();
    }
    directories[..directories.len().min(depth)].join("/")
}

/// Share of values less than or equal to `value`.
fn percentile_rank(values: &[f64], value: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().filter(|v| **v <= value).count() as f64 / values.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_uses_directory_prefix_up_to_depth() {
        assert_eq!(
            module_of("apps/billing/lib/a.ex", 2),
            "apps/billing",
            "should cut at depth 2"
        );
        assert_eq!(
            module_of("apps/billing/test/a_test.exs", 2),
            "apps/billing",
            "should put lib and test files of one app in the same module"
        );
        assert_eq!(
            module_of("lib/a.ex", 2),
            "lib",
            "should use shallower directories as they are"
        );
        assert_eq!(
            module_of("README.md", 2),
            ".",
            "should group root files in the root module"
        );
    }

    #[test]
    fn breakdown_shares_and_efficiency() {
        let work = WorkTypes {
            new_work: 600,
            legacy_refactor: 200,
            help_others: 100,
            churn: 100,
        };
        assert_eq!(
            work.shares(),
            Some([0.6, 0.2, 0.1, 0.1]),
            "should split 60%, 20%, 10% and 10%"
        );
        assert_eq!(
            work.efficiency(),
            Some(0.9),
            "should be 90% when churn is 100 of 1,000"
        );
        assert_eq!(
            WorkTypes::default().shares(),
            None,
            "should have no shares without changes"
        );
    }

    #[test]
    fn percentile_rank_counts_values_at_or_below() {
        let values = [10.0, 20.0, 20.0, 40.0];
        assert_eq!(
            percentile_rank(&values, 20.0),
            0.75,
            "should count ties as at or below"
        );
        assert_eq!(
            percentile_rank(&values, 40.0),
            1.0,
            "should rank the maximum at 1.0"
        );
        assert_eq!(
            percentile_rank(&values, 10.0),
            0.25,
            "should rank the minimum above zero"
        );
    }
}
