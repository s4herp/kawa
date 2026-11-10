//! Review and collaboration metrics derived from stored pull requests.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use rusqlite::{OptionalExtension, params};

use crate::fundamentals::Selection;
use crate::{Result, Store, StoreError};

/// Median, 75th percentile and sample count, in seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aggregate {
    pub median: f64,
    pub p75: f64,
    pub n: u64,
}

/// Median by averaging the two middle values; p75 by nearest rank.
pub fn aggregate(values: &mut [i64]) -> Option<Aggregate> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let n = values.len();
    let median = if n % 2 == 1 {
        values[n / 2] as f64
    } else {
        (values[n / 2 - 1] + values[n / 2]) as f64 / 2.0
    };
    let rank = (0.75 * n as f64).ceil() as usize;
    Some(Aggregate {
        median,
        p75: values[rank.max(1) - 1] as f64,
        n: n as u64,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuthorReview {
    pub developer_id: String,
    pub name: String,
    pub pull_requests: u64,
    pub merged: u64,
    pub unreviewed: u64,
    pub cycle: Option<Aggregate>,
    pub coding: Option<Aggregate>,
    pub pickup: Option<Aggregate>,
    pub review: Option<Aggregate>,
    pub responsiveness: Option<Aggregate>,
    pub iterations: u64,
}

impl AuthorReview {
    pub fn unreviewed_share(&self) -> Option<f64> {
        (self.merged > 0).then(|| self.unreviewed as f64 / self.merged as f64)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReviewerStats {
    pub developer_id: String,
    pub name: String,
    pub pull_requests_reviewed: u64,
    pub reaction: Option<Aggregate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixCell {
    pub reviewer_id: String,
    pub author_id: String,
    pub pull_requests: Vec<(i64, i64)>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReviewSummary {
    pub authors: Vec<AuthorReview>,
    pub reviewers: Vec<ReviewerStats>,
    pub matrix: Vec<MatrixCell>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrEvent {
    pub at: Timestamp,
    pub kind: String,
    pub actor: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrTimings {
    pub first_commit_at: Option<Timestamp>,
    pub ready_at: Option<Timestamp>,
    pub first_response_at: Option<Timestamp>,
    pub merged_at: Option<Timestamp>,
    pub coding_seconds: Option<i64>,
    pub pickup_seconds: Option<i64>,
    pub review_seconds: Option<i64>,
    pub cycle_seconds: Option<i64>,
    pub iterations: u64,
    pub is_unreviewed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrDetail {
    pub repository_id: i64,
    pub number: i64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub timings: PrTimings,
    /// Every event used, oldest first.
    pub events: Vec<PrEvent>,
}

/// Everything stored about one pull request, with logins already resolved
/// to developer ids (`None` for excluded identities such as bots).
struct PrData {
    repository_id: i64,
    number: i64,
    author: Option<String>,
    created_at: Timestamp,
    merged_at: Option<Timestamp>,
    closed_at: Option<Timestamp>,
    is_draft: bool,
    commits: Vec<(Timestamp, Timestamp, Option<String>)>,
    responses: Vec<Response>,
    draft_events: Vec<(Timestamp, bool)>,
    requests: Vec<(Timestamp, Option<String>)>,
}

struct Response {
    at: Timestamp,
    developer: Option<String>,
    is_review: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Sample {
    kind: &'static str,
    developer: String,
    started_at: Timestamp,
    seconds: i64,
}

fn seconds(from: Option<Timestamp>, to: Option<Timestamp>) -> Option<i64> {
    Some(to?.as_second() - from?.as_second()).filter(|s| *s >= 0)
}

impl PrData {
    /// Opened ready unless the first draft-related event is a "ready" one or
    /// the PR is still a draft with no such events.
    fn ready_at(&self) -> Option<Timestamp> {
        let mut events = self.draft_events.clone();
        events.sort();
        let opened_as_draft = match events.first() {
            Some((_, became_ready)) => *became_ready,
            None => self.is_draft,
        };
        if !opened_as_draft {
            return Some(self.created_at);
        }
        events.iter().find(|(_, ready)| *ready).map(|(at, _)| *at)
    }

    fn is_other(&self, developer: &Option<String>) -> bool {
        developer.is_some() && *developer != self.author
    }

    fn compute(&self) -> (PrTimings, Vec<Sample>) {
        let first_commit_at = self.commits.iter().map(|(authored, _, _)| *authored).min();
        let ready_at = self.ready_at();
        let mut others: Vec<&Response> = self
            .responses
            .iter()
            .filter(|r| self.is_other(&r.developer))
            .collect();
        others.sort_by_key(|r| r.at);
        let first_response_at = others
            .iter()
            .map(|r| r.at)
            .find(|at| ready_at.is_some_and(|ready| *at >= ready));
        let is_unreviewed = self.merged_at.is_some() && others.is_empty();
        let merged_at = self.merged_at;

        let mut timeline: Vec<(Timestamp, u8, Option<String>)> = Vec::new();
        for (_, committed, developer) in &self.commits {
            timeline.push((*committed, 0, developer.clone()));
        }
        for response in &self.responses {
            timeline.push((
                response.at,
                if response.is_review { 1 } else { 2 },
                response.developer.clone(),
            ));
        }
        timeline.sort();
        let mut iterations = 0;
        let mut reviewed_since_commit = false;
        let mut pending_since: Option<Timestamp> = None;
        let mut samples = Vec::new();
        for (at, kind, developer) in &timeline {
            let by_author = developer.is_some() && *developer == self.author;
            if self.is_other(developer) {
                if *kind == 1 {
                    reviewed_since_commit = true;
                }
                pending_since.get_or_insert(*at);
            } else if by_author {
                if *kind == 0 && reviewed_since_commit {
                    iterations += 1;
                    reviewed_since_commit = false;
                }
                if let (Some(since), Some(author)) = (pending_since.take(), &self.author) {
                    samples.push(Sample {
                        kind: "responsiveness",
                        developer: author.clone(),
                        started_at: since,
                        seconds: at.as_second() - since.as_second(),
                    });
                }
            }
            if merged_at.is_some_and(|m| *at > m) {
                break;
            }
        }

        let mut reviewers: BTreeSet<String> =
            others.iter().filter_map(|r| r.developer.clone()).collect();
        reviewers.extend(self.requests.iter().filter_map(|(_, d)| d.clone()));
        for reviewer in reviewers {
            if Some(&reviewer) == self.author.as_ref() {
                continue;
            }
            let start = self
                .requests
                .iter()
                .filter(|(_, d)| d.as_ref() == Some(&reviewer))
                .map(|(at, _)| *at)
                .min()
                .or(ready_at);
            let Some(start) = start else { continue };
            let reaction = others
                .iter()
                .filter(|r| r.developer.as_ref() == Some(&reviewer) && r.at >= start)
                .map(|r| r.at)
                .min();
            if let Some(end) = reaction {
                samples.push(Sample {
                    kind: "reaction",
                    developer: reviewer,
                    started_at: start,
                    seconds: end.as_second() - start.as_second(),
                });
            }
        }

        let timings = PrTimings {
            first_commit_at,
            ready_at,
            first_response_at,
            merged_at,
            coding_seconds: seconds(first_commit_at, ready_at),
            pickup_seconds: seconds(ready_at, first_response_at),
            review_seconds: seconds(first_response_at, merged_at),
            cycle_seconds: seconds(first_commit_at, merged_at),
            iterations,
            is_unreviewed,
        };
        (timings, samples)
    }
}

fn parse(column: &'static str, value: String) -> Result<Timestamp> {
    value
        .parse()
        .map_err(|_| StoreError::InvalidValue { column, value })
}

fn parse_opt(column: &'static str, value: Option<String>) -> Result<Option<Timestamp>> {
    value.map(|v| parse(column, v)).transpose()
}

impl Store {
    /// Login to developer id for every resolved login; excluded logins map to `None`.
    fn login_developers(&self) -> Result<HashMap<String, Option<String>>> {
        let mut statement = self
            .conn()
            .prepare("SELECT login, developer_id, excluded FROM identities WHERE login <> ''")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let (login, developer, excluded) = row?;
            map.insert(login, if excluded { None } else { developer });
        }
        Ok(map)
    }

    fn load_prs(&self, only: Option<(i64, i64)>) -> Result<Vec<PrData>> {
        let logins = self.login_developers()?;
        let developer = |login: Option<String>| -> Option<String> {
            let login = login?;
            match logins.get(&login) {
                Some(resolved) => resolved.clone(),
                None => Some(format!("gh:{}", login.to_lowercase())),
            }
        };
        let filter = if only.is_some() {
            " WHERE repository_id = ?1 AND number = ?2"
        } else {
            ""
        };
        let mut statement = self.conn().prepare(&format!(
            "SELECT repository_id, number, author_login, created_at, merged_at, closed_at, is_draft
             FROM pull_requests{filter} ORDER BY repository_id, number"
        ))?;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, bool>(6)?,
            ))
        };
        let rows: Vec<_> = match only {
            Some((repository_id, number)) => statement
                .query_map(params![repository_id, number], map_row)?
                .collect::<rusqlite::Result<_>>()?,
            None => statement
                .query_map([], map_row)?
                .collect::<rusqlite::Result<_>>()?,
        };

        let mut prs = Vec::new();
        let mut commits = self.conn().prepare(
            "SELECT authored_at, committed_at, author_login FROM pr_commits
             WHERE repository_id = ?1 AND number = ?2",
        )?;
        let mut reviews = self.conn().prepare(
            "SELECT submitted_at, author_login FROM reviews WHERE repository_id = ?1 AND number = ?2",
        )?;
        let mut comments = self.conn().prepare(
            "SELECT created_at, author_login FROM comments WHERE repository_id = ?1 AND number = ?2",
        )?;
        let mut events = self.conn().prepare(
            "SELECT kind, at, subject_login FROM pr_events
             WHERE repository_id = ?1 AND number = ?2 ORDER BY seq",
        )?;
        for (repository_id, number, author_login, created_at, merged_at, closed_at, is_draft) in
            rows
        {
            let key = params![repository_id, number];
            let mut pr = PrData {
                repository_id,
                number,
                author: developer(Some(author_login)),
                created_at: parse("created_at", created_at)?,
                merged_at: parse_opt("merged_at", merged_at)?,
                closed_at: parse_opt("closed_at", closed_at)?,
                is_draft,
                commits: Vec::new(),
                responses: Vec::new(),
                draft_events: Vec::new(),
                requests: Vec::new(),
            };
            for row in commits.query_map(key, |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })? {
                let (authored, committed, login) = row?;
                // Commits pushed by the PR author whose login GitHub could not
                // match still belong to the author.
                let by = login.map_or_else(|| pr.author.clone(), |l| developer(Some(l)));
                pr.commits.push((
                    parse("authored_at", authored)?,
                    parse("committed_at", committed)?,
                    by,
                ));
            }
            for (statement, is_review) in [(&mut reviews, true), (&mut comments, false)] {
                for row in statement.query_map(key, |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })? {
                    let (at, login) = row?;
                    pr.responses.push(Response {
                        at: parse("response_at", at)?,
                        developer: developer(Some(login)),
                        is_review,
                    });
                }
            }
            for row in events.query_map(key, |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })? {
                let (kind, at, subject) = row?;
                let at = parse("pr_events.at", at)?;
                match kind.as_str() {
                    "ready_for_review" => pr.draft_events.push((at, true)),
                    "convert_to_draft" => pr.draft_events.push((at, false)),
                    "review_requested" => pr.requests.push((at, developer(subject))),
                    _ => {}
                }
            }
            prs.push(pr);
        }
        Ok(prs)
    }

    /// Rebuilds `pr_timings` and `pr_response_samples` for merged or closed
    /// PRs whose author is not excluded.
    pub fn rebuild_pr_timings(&mut self, timezone: &TimeZone) -> Result<()> {
        let prs = self.load_prs(None)?;
        let tx = self.conn_mut().transaction()?;
        tx.execute("DELETE FROM pr_timings", [])?;
        tx.execute("DELETE FROM pr_response_samples", [])?;
        {
            let mut timing = tx.prepare(
                "INSERT INTO pr_timings (repository_id, number, author_developer_id, day, is_merged,
                   first_commit_at, ready_at, first_response_at, merged_at, coding_seconds,
                   pickup_seconds, review_seconds, cycle_seconds, iterations, is_unreviewed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )?;
            let mut sample = tx.prepare(
                "INSERT INTO pr_response_samples (repository_id, number, kind, developer_id, started_at, seconds)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            let text = |t: Option<Timestamp>| t.map(|t| t.to_string());
            for pr in prs {
                let (Some(author), Some(finished)) = (&pr.author, pr.merged_at.or(pr.closed_at))
                else {
                    continue;
                };
                let (t, samples) = pr.compute();
                timing.execute(params![
                    pr.repository_id,
                    pr.number,
                    author,
                    finished.to_zoned(timezone.clone()).date().to_string(),
                    pr.merged_at.is_some(),
                    text(t.first_commit_at),
                    text(t.ready_at),
                    text(t.first_response_at),
                    text(t.merged_at),
                    t.coding_seconds,
                    t.pickup_seconds,
                    t.review_seconds,
                    t.cycle_seconds,
                    t.iterations as i64,
                    t.is_unreviewed,
                ])?;
                for s in samples {
                    sample.execute(params![
                        pr.repository_id,
                        pr.number,
                        s.kind,
                        s.developer,
                        s.started_at.to_string(),
                        s.seconds,
                    ])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Author, reviewer and matrix metrics for PRs merged or closed in the
    /// selection, sorted by display name.
    pub fn review_summary(&self, selection: &Selection) -> Result<ReviewSummary> {
        let names = self.developer_names()?;
        let name = |id: &str| {
            names
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.trim_start_matches("gh:").to_owned())
        };
        let range = params![selection.start.to_string(), selection.end.to_string()];

        #[derive(Default)]
        struct Acc {
            prs: u64,
            merged: u64,
            unreviewed: u64,
            cycle: Vec<i64>,
            coding: Vec<i64>,
            pickup: Vec<i64>,
            review: Vec<i64>,
            responsiveness: Vec<i64>,
            iterations: u64,
        }
        let mut authors: BTreeMap<String, Acc> = BTreeMap::new();
        let mut in_scope: BTreeSet<(i64, i64)> = BTreeSet::new();
        let mut pr_author: HashMap<(i64, i64), String> = HashMap::new();
        {
            let mut statement = self.conn().prepare(
                "SELECT repository_id, number, author_developer_id, is_merged, coding_seconds,
                        pickup_seconds, review_seconds, cycle_seconds, iterations, is_unreviewed
                 FROM pr_timings WHERE day BETWEEN ?1 AND ?2",
            )?;
            let rows = statement.query_map(range, |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, bool>(3)?,
                    [
                        r.get::<_, Option<i64>>(4)?,
                        r.get::<_, Option<i64>>(5)?,
                        r.get::<_, Option<i64>>(6)?,
                        r.get::<_, Option<i64>>(7)?,
                    ],
                    r.get::<_, u64>(8)?,
                    r.get::<_, bool>(9)?,
                ))
            })?;
            for row in rows {
                let (
                    repository_id,
                    number,
                    author,
                    merged,
                    [coding, pickup, review, cycle],
                    iterations,
                    unreviewed,
                ) = row?;
                if !selection.repositories.is_empty()
                    && !selection.repositories.contains(&repository_id)
                {
                    continue;
                }
                in_scope.insert((repository_id, number));
                pr_author.insert((repository_id, number), author.clone());
                if !selection.developers.is_empty() && !selection.developers.contains(&author) {
                    continue;
                }
                let acc = authors.entry(author).or_default();
                acc.prs += 1;
                acc.merged += u64::from(merged);
                acc.unreviewed += u64::from(unreviewed);
                acc.iterations += iterations;
                if merged {
                    acc.coding.extend(coding);
                    acc.pickup.extend(pickup);
                    acc.review.extend(review);
                    acc.cycle.extend(cycle);
                }
            }
        }

        let mut reactions: BTreeMap<String, Vec<i64>> = BTreeMap::new();
        {
            let mut statement = self.conn().prepare(
                "SELECT repository_id, number, kind, developer_id, seconds FROM pr_response_samples",
            )?;
            let rows = statement.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?;
            for row in rows {
                let (repository_id, number, kind, developer, secs) = row?;
                if !in_scope.contains(&(repository_id, number)) {
                    continue;
                }
                if kind == "responsiveness" {
                    if let Some(acc) = authors.get_mut(&developer) {
                        acc.responsiveness.push(secs);
                    }
                } else if selection.developers.is_empty()
                    || selection.developers.contains(&developer)
                {
                    reactions.entry(developer).or_default().push(secs);
                }
            }
        }

        let mut reviewed: BTreeMap<(String, String), BTreeSet<(i64, i64)>> = BTreeMap::new();
        let logins = self.login_developers()?;
        {
            let mut statement = self
                .conn()
                .prepare("SELECT repository_id, number, author_login FROM reviews")?;
            let rows = statement.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (repository_id, number, login) = row?;
                let Some(author) = pr_author.get(&(repository_id, number)) else {
                    continue;
                };
                let reviewer = match logins.get(&login) {
                    Some(Some(id)) => id.clone(),
                    Some(None) => continue,
                    None => format!("gh:{}", login.to_lowercase()),
                };
                if &reviewer == author {
                    continue;
                }
                let selected = |id: &String| {
                    selection.developers.is_empty() || selection.developers.contains(id)
                };
                if selected(&reviewer) && selected(author) {
                    reviewed
                        .entry((reviewer, author.clone()))
                        .or_default()
                        .insert((repository_id, number));
                }
            }
        }

        let by_name = |a: &String, b: &String| {
            name(a)
                .to_lowercase()
                .cmp(&name(b).to_lowercase())
                .then_with(|| a.cmp(b))
        };
        let mut summary = ReviewSummary {
            authors: authors
                .into_iter()
                .map(|(id, mut acc)| AuthorReview {
                    name: name(&id),
                    developer_id: id,
                    pull_requests: acc.prs,
                    merged: acc.merged,
                    unreviewed: acc.unreviewed,
                    cycle: aggregate(&mut acc.cycle),
                    coding: aggregate(&mut acc.coding),
                    pickup: aggregate(&mut acc.pickup),
                    review: aggregate(&mut acc.review),
                    responsiveness: aggregate(&mut acc.responsiveness),
                    iterations: acc.iterations,
                })
                .collect(),
            reviewers: Vec::new(),
            matrix: reviewed
                .iter()
                .map(|((reviewer, author), prs)| MatrixCell {
                    reviewer_id: reviewer.clone(),
                    author_id: author.clone(),
                    pull_requests: prs.iter().copied().collect(),
                })
                .collect(),
        };
        let mut reviewer_ids: BTreeSet<String> = reactions.keys().cloned().collect();
        reviewer_ids.extend(reviewed.keys().map(|(r, _)| r.clone()));
        summary.reviewers = reviewer_ids
            .into_iter()
            .map(|id| ReviewerStats {
                name: name(&id),
                pull_requests_reviewed: reviewed
                    .iter()
                    .filter(|((r, _), _)| *r == id)
                    .map(|(_, prs)| prs.len() as u64)
                    .sum(),
                reaction: reactions.get_mut(&id).and_then(|v| aggregate(v)),
                developer_id: id,
            })
            .collect();
        summary
            .authors
            .sort_by(|a, b| by_name(&a.developer_id, &b.developer_id));
        summary
            .reviewers
            .sort_by(|a, b| by_name(&a.developer_id, &b.developer_id));
        Ok(summary)
    }

    /// One PR with its timings and every event they were computed from.
    pub fn pr_detail(&self, repository_id: i64, number: i64) -> Result<Option<PrDetail>> {
        let Some((title, url, author)) = self
            .conn()
            .query_row(
                "SELECT title, url, author_login FROM pull_requests WHERE repository_id = ?1 AND number = ?2",
                params![repository_id, number],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let timings = self
            .load_prs(Some((repository_id, number)))?
            .first()
            .map(|pr| pr.compute().0)
            .unwrap_or_default();
        let mut events = Vec::new();
        let key = params![repository_id, number];
        let sources = [
            "SELECT committed_at, 'commit', author_login, sha FROM pr_commits WHERE repository_id = ?1 AND number = ?2",
            "SELECT submitted_at, 'review', author_login, state FROM reviews WHERE repository_id = ?1 AND number = ?2",
            "SELECT created_at, kind || '_comment', author_login, NULL FROM comments WHERE repository_id = ?1 AND number = ?2",
            "SELECT at, kind, actor_login, subject_login FROM pr_events WHERE repository_id = ?1 AND number = ?2",
        ];
        for sql in sources {
            let mut statement = self.conn().prepare(sql)?;
            for row in statement.query_map(key, |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })? {
                let (at, kind, actor, detail) = row?;
                events.push(PrEvent {
                    at: parse("event_at", at)?,
                    kind,
                    actor,
                    detail,
                });
            }
        }
        events.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.kind.cmp(&b.kind)));
        Ok(Some(PrDetail {
            repository_id,
            number,
            title,
            url,
            author,
            timings,
            events,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_uses_middle_average_and_nearest_rank() {
        let mut odd = [5, 1, 3];
        assert_eq!(
            aggregate(&mut odd),
            Some(Aggregate {
                median: 3.0,
                p75: 5.0,
                n: 3
            }),
            "should take the middle value and the nearest-rank p75"
        );
        let mut even = [1, 2, 3, 4];
        assert_eq!(
            aggregate(&mut even),
            Some(Aggregate {
                median: 2.5,
                p75: 3.0,
                n: 4
            }),
            "should average the two middle values"
        );
        assert_eq!(
            aggregate(&mut []),
            None,
            "should have no aggregate without samples"
        );
    }
}
