//! Page handlers. Every metric value is rendered with `metric`, a link to the
//! evidence behind it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use axum::extract::State;
use axum_extra::extract::Query;
use jiff::civil::{Date, Weekday};
use maud::{Markup, html};
use serde::Deserialize;

use kawa_store::{Aggregate, ItemKind, Selection, Store, WorkType, WorkTypes};

use super::charts::{self, Point};
use super::layout::{self, Page};
use super::{AppState, Filters, MAX_WORK_LOG_DAYS};
use crate::identities;

type S = State<Arc<AppState>>;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Extra {
    id: Option<String>,
    metric: Option<String>,
    day: Option<String>,
    author: Option<String>,
    reviewer: Option<String>,
    repository: Option<i64>,
    number: Option<i64>,
}

fn metric(href: String, text: impl Into<String>) -> Markup {
    html! { a.metric href=(href) { (text.into()) } }
}

fn pct(value: f64) -> String {
    format!("{:.0}%", value * 100.0)
}

fn hours(seconds: f64) -> String {
    format!("{:.1} h", seconds / 3600.0)
}

fn agg(value: Option<Aggregate>) -> String {
    match value {
        Some(a) => format!("{} · p75 {} · n {}", hours(a.median), hours(a.p75), a.n),
        None => "–".into(),
    }
}

fn names(store: &Store) -> HashMap<String, String> {
    store.developer_names().unwrap_or_default()
}

fn name_of(names: &HashMap<String, String>, id: &str) -> String {
    names
        .get(id)
        .cloned()
        .unwrap_or_else(|| id.trim_start_matches("gh:").to_owned())
}

fn error(message: impl std::fmt::Display) -> Markup {
    html! { p.notice { "Could not load this view: " (message.to_string()) } }
}

struct Ctx {
    state: Arc<AppState>,
    filters: Filters,
    selection: Selection,
}

impl Ctx {
    fn new(state: Arc<AppState>, filters: Filters) -> Self {
        let selection = filters.selection(state.today());
        Self {
            state,
            filters,
            selection,
        }
    }

    fn page(
        &self,
        store: &Store,
        title: &str,
        path: &str,
        keep: &[(&str, String)],
        body: Markup,
    ) -> Markup {
        let page = Page {
            title,
            path,
            filters: &self.filters,
            selection: &self.selection,
            keep,
        };
        layout::render(store, (self.state.clock)(), &page, body)
    }
}

pub async fn overview(State(state): S, Query(filters): Query<Filters>) -> Markup {
    let ctx = Ctx::new(state, filters);
    let store = ctx.state.store.lock().expect("store lock");
    let settings = ctx.state.config.impact_settings();
    let body = match store.fundamentals(&ctx.selection, &settings) {
        Err(e) => error(e),
        Ok(rows) => {
            let link = |id: &str, metric: &str| {
                ctx.filters
                    .link("/evidence", &[("id", id), ("metric", metric)])
            };
            let points: Vec<Point> = rows
                .iter()
                .filter_map(|r| {
                    Some(Point {
                        label: r.name.clone(),
                        link: ctx.filters.link("/developer", &[("id", &r.developer_id)]),
                        x: r.impact?,
                        y: r.efficiency?,
                    })
                })
                .collect();
            let insufficient: Vec<&str> = rows
                .iter()
                .filter(|r| r.impact.is_none())
                .map(|r| r.name.as_str())
                .collect();
            html! {
                @if rows.is_empty() {
                    p { "No commits in this period." }
                } @else {
                    (charts::quadrant(&points))
                    p.note {
                        "Each point is a developer's pattern of activity in this selection, split at the medians. "
                        "This chart is not a performance evaluation: design, incidents, reviews and helping others "
                        "often leave few lines behind."
                    }
                    @if !insufficient.is_empty() {
                        p.note { "Not plotted, insufficient data (fewer than " (settings.min_lines) " changed lines): " (insufficient.join(", ")) }
                    }
                    div.scroll { table {
                        thead { tr {
                            th { "Developer" } th.num { "Active days" } th.num { "Commits per active day" }
                            th.num { "New work" } th.num { "Legacy refactor" } th.num { "Helping others" }
                            th.num { "Churn" } th.num { "Efficiency" } th.num { "Impact" }
                        } }
                        tbody {
                            @for r in &rows {
                                tr {
                                    td { a href=(ctx.filters.link("/developer", &[("id", &r.developer_id)])) { (r.name) } }
                                    td.num { (metric(link(&r.developer_id, "commits"), r.active_days.to_string())) }
                                    td.num { (metric(link(&r.developer_id, "commits"), format!("{:.1}", r.commits_per_active_day))) }
                                    td.num { (metric(link(&r.developer_id, "new_work"), r.work.new_work.to_string())) }
                                    td.num { (metric(link(&r.developer_id, "legacy_refactor"), r.work.legacy_refactor.to_string())) }
                                    td.num { (metric(link(&r.developer_id, "help_others"), r.work.help_others.to_string())) }
                                    td.num { (metric(link(&r.developer_id, "churn"), r.work.churn.to_string())) }
                                    td.num { (metric(link(&r.developer_id, "churn"), r.efficiency.map_or("insufficient data".into(), pct))) }
                                    td.num { (metric(link(&r.developer_id, "impact"), r.impact.map_or("insufficient data".into(), |v| format!("{v:.2}")))) }
                                }
                            }
                        }
                    } }
                    p.note {
                        "Impact weights: lines " (settings.lines) ", files " (settings.files) ", modules " (settings.modules)
                        ". Impact is relative to the selected developers and not comparable across selections."
                    }
                }
            }
        }
    };
    ctx.page(&store, "Code fundamentals", "/", &[], body)
}

pub async fn evidence(
    State(state): S,
    Query(filters): Query<Filters>,
    Query(extra): Query<Extra>,
) -> Markup {
    let ctx = Ctx::new(state, filters);
    let store = ctx.state.store.lock().expect("store lock");
    let id = extra.id.clone().unwrap_or_default();
    let names = names(&store);
    let body = match store.fundamentals_evidence(&ctx.selection, &id, ctx.state.config.recency_days)
    {
        Err(e) => error(e),
        Ok(commits) => {
            let total = commits.iter().fold(WorkTypes::default(), |mut t, c| {
                t.new_work += c.work.new_work;
                t.legacy_refactor += c.work.legacy_refactor;
                t.help_others += c.work.help_others;
                t.churn += c.work.churn;
                t
            });
            let github: HashMap<i64, String> = store
                .repositories()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|r| r.github.map(|g| (r.id, g)))
                .collect();
            html! {
                p { "Commits by " strong { (name_of(&names, &id)) } " in this selection"
                    @if let Some(m) = &extra.metric { " (showing evidence for " (m.replace('_', " ")) ")" } "." }
                table {
                    thead { tr { th { "Day" } th { "Commit" } th.num { "New work" } th.num { "Legacy refactor" }
                                 th.num { "Helping others" } th.num { "Churn" } th { "Rewrote lines from" } } }
                    tbody {
                        @for c in &commits {
                            tr {
                                td { (c.day) }
                                td {
                                    @if let Some(g) = github.get(&c.repository_id) {
                                        a href=(format!("https://github.com/{g}/commit/{}", c.sha)) { code { (&c.sha[..10]) } }
                                    } @else { code { (&c.sha[..10]) } }
                                    " " (c.subject)
                                }
                                td.num { (c.work.new_work) } td.num { (c.work.legacy_refactor) }
                                td.num { (c.work.help_others) } td.num { (c.work.churn) }
                                td {
                                    @for r in &c.rewrote {
                                        div {
                                            (r.lines) " lines, "
                                            (match r.work_type { WorkType::Churn => "churn", WorkType::HelpOthers => "helping others",
                                                                 WorkType::LegacyRefactor => "legacy refactor", WorkType::NewWork => "new work" })
                                            ", from "
                                            @match (&r.introduced_sha, &r.introduced_by) {
                                                (Some(sha), Some(by)) => { code { (&sha[..10]) } " by " (name_of(&names, by)) }
                                                (Some(sha), None) => { code { (&sha[..10]) } " (excluded identity)" }
                                                _ => { "before the analyzed history" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        tr { th { "Total" } th {} th.num { (total.new_work) } th.num { (total.legacy_refactor) }
                             th.num { (total.help_others) } th.num { (total.churn) } th {} }
                    }
                }
            }
        }
    };
    let keep = [
        ("id", id.clone()),
        ("metric", extra.metric.clone().unwrap_or_default()),
    ];
    ctx.page(&store, "Evidence", "/evidence", &keep, body)
}

pub async fn developer(
    State(state): S,
    Query(filters): Query<Filters>,
    Query(extra): Query<Extra>,
) -> Markup {
    let ctx = Ctx::new(state, filters);
    let store = ctx.state.store.lock().expect("store lock");
    let id = extra.id.clone().unwrap_or_default();
    let names = names(&store);
    let link = |metric: &str| {
        ctx.filters
            .link("/evidence", &[("id", &id), ("metric", metric)])
    };
    let fundamentals = store
        .fundamentals(&ctx.selection, &ctx.state.config.impact_settings())
        .unwrap_or_default()
        .into_iter()
        .find(|r| r.developer_id == id);
    let weeks = store.weekly_work(&ctx.selection, &id).unwrap_or_default();
    let reviews = store.review_summary(&ctx.selection).unwrap_or_default();
    let as_author = reviews.authors.iter().find(|a| a.developer_id == id);
    let as_reviewer = reviews.reviewers.iter().find(|r| r.developer_id == id);
    let body = html! {
        h2 { (name_of(&names, &id)) }
        @if let Some(r) = &fundamentals {
            table { tbody {
                tr { th { "Active days" } td.num { (metric(link("commits"), r.active_days.to_string())) } }
                tr { th { "Commits" } td.num { (metric(link("commits"), r.commits.to_string())) } }
                tr { th { "Efficiency" } td.num { (metric(link("churn"), r.efficiency.map_or("insufficient data".into(), pct))) } }
                tr { th { "Impact" } td.num { (metric(link("impact"), r.impact.map_or("insufficient data".into(), |v| format!("{v:.2}")))) } }
            } }
        } @else {
            p { "No commits in this period." }
        }
        h3 { "Work types per week" }
        table {
            thead { tr { th { "Week of" } th.num { "New work" } th.num { "Legacy refactor" } th.num { "Helping others" } th.num { "Churn" } } }
            tbody {
                @for (week, w) in &weeks {
                    tr {
                        td { (week.to_string()) }
                        @for (value, m) in [(w.new_work, "new_work"), (w.legacy_refactor, "legacy_refactor"), (w.help_others, "help_others"), (w.churn, "churn")] {
                            td.num { (metric(link(m), value.to_string())) }
                        }
                    }
                }
            }
        }
        h3 { "Pull requests" }
        @if let Some(a) = as_author {
            @let prs = ctx.filters.link("/reviews/prs", &[("author", &id)]);
            table { tbody {
                tr { th { "Merged" } td.num { (metric(prs.clone(), a.merged.to_string())) } }
                tr { th { "Cycle time" } td.num { (metric(prs.clone(), agg(a.cycle))) } }
                tr { th { "Pickup" } td.num { (metric(prs.clone(), agg(a.pickup))) } }
                tr { th { "Responsiveness" } td.num { (metric(prs.clone(), agg(a.responsiveness))) } }
                tr { th { "Unreviewed" } td.num { (metric(prs, a.unreviewed.to_string())) } }
            } }
        } @else { p { "No pull requests merged or closed in this period." } }
        @if let Some(r) = as_reviewer {
            @let prs = ctx.filters.link("/reviews/prs", &[("reviewer", &id)]);
            table { tbody {
                tr { th { "PRs reviewed" } td.num { (metric(prs.clone(), r.pull_requests_reviewed.to_string())) } }
                tr { th { "Reaction time" } td.num { (metric(prs, agg(r.reaction))) } }
            } }
        }
    };
    ctx.page(
        &store,
        "Developer",
        "/developer",
        &[("id", id.clone())],
        body,
    )
}

fn days_between(start: Date, end: Date) -> Vec<Date> {
    let mut days = Vec::new();
    let mut day = start;
    while day <= end {
        days.push(day);
        match day.tomorrow() {
            Ok(next) => day = next,
            Err(_) => break,
        }
    }
    days
}

pub async fn work_log(State(state): S, Query(filters): Query<Filters>) -> Markup {
    let ctx = Ctx::new(state, filters);
    let store = ctx.state.store.lock().expect("store lock");
    let length = ctx
        .selection
        .start
        .until(ctx.selection.end)
        .map(|s| s.get_days() + 1)
        .unwrap_or(0);
    let body = if length > MAX_WORK_LOG_DAYS {
        html! { p.notice { "The work log shows up to " (MAX_WORK_LOG_DAYS) " days. Choose a shorter period." } }
    } else {
        match store.work_log(&ctx.selection, &ctx.state.config.timezone()) {
            Err(e) => error(e),
            Ok(grid) => {
                let names = names(&store);
                let days = days_between(ctx.selection.start, ctx.selection.end);
                let mut developers: Vec<&String> = grid.keys().collect();
                developers.sort_by_key(|id| (name_of(&names, id).to_lowercase(), (*id).clone()));
                html! {
                    p.note { "C commits · O pull requests opened · M merged · R reviews · K comments. Empty days carry no meaning." }
                    div.scroll { table.grid {
                        thead { tr {
                            th { "Developer" }
                            @for d in &days {
                                th.weekend[matches!(d.weekday(), Weekday::Saturday | Weekday::Sunday)] { (d.strftime("%a %d").to_string()) }
                            }
                        } }
                        tbody {
                            @for id in &developers {
                                tr {
                                    td { a href=(ctx.filters.link("/developer", &[("id", id)])) { (name_of(&names, id)) } }
                                    @for d in &days {
                                        @let weekend = matches!(d.weekday(), Weekday::Saturday | Weekday::Sunday);
                                        @let key = d.to_string();
                                        @match grid[*id].get(&key) {
                                            Some(a) if !a.is_empty() => {
                                                td.weekend[weekend] {
                                                    @let parts: Vec<String> = [("C", a.commits), ("O", a.prs_opened), ("M", a.prs_merged), ("R", a.reviews), ("K", a.comments)]
                                                        .iter().filter(|(_, n)| *n > 0).map(|(l, n)| format!("{l}{n}")).collect();
                                                    (metric(ctx.filters.link("/work-log/day", &[("id", id), ("day", &key)]), parts.join(" ")))
                                                }
                                            }
                                            _ => { td.weekend[weekend] {} }
                                        }
                                    }
                                }
                            }
                        }
                    } }
                }
            }
        }
    };
    ctx.page(&store, "Work log", "/work-log", &[], body)
}

pub async fn work_log_day(
    State(state): S,
    Query(filters): Query<Filters>,
    Query(extra): Query<Extra>,
) -> Markup {
    let ctx = Ctx::new(state, filters);
    let store = ctx.state.store.lock().expect("store lock");
    let id = extra.id.clone().unwrap_or_default();
    let day = extra.day.clone().unwrap_or_default();
    let body = match day.parse::<Date>() {
        Err(_) => error("invalid day"),
        Ok(date) => {
            let selection = Selection {
                start: date,
                end: date,
                repositories: ctx.selection.repositories.clone(),
                developers: BTreeSet::from([id.clone()]),
            };
            match store.work_log_items(&selection, &ctx.state.config.timezone()) {
                Err(e) => error(e),
                Ok(items) => html! {
                    p { strong { (name_of(&names(&store), &id)) } " on " (day) }
                    table {
                        thead { tr { th { "Time" } th { "Type" } th { "Item" } th { "Detail" } } }
                        tbody {
                            @for item in &items {
                                tr {
                                    td { (item.at.to_zoned(ctx.state.config.timezone()).strftime("%H:%M").to_string()) }
                                    td { (match &item.kind {
                                        ItemKind::Commit { .. } => "Commit",
                                        ItemKind::PullRequestOpened { .. } => "Opened PR",
                                        ItemKind::PullRequestMerged { .. } => "Merged PR",
                                        ItemKind::Review { .. } => "Review",
                                        ItemKind::Comment { .. } => "Comment",
                                    }) }
                                    td {
                                        @if let Some(url) = &item.url { a href=(url) { (item.title) } } @else { (item.title) }
                                    }
                                    td {
                                        @match &item.kind {
                                            ItemKind::Commit { lines_added, lines_removed, work, .. } => {
                                                "+" (lines_added) " −" (lines_removed) ": new work " (work.new_work)
                                                ", legacy refactor " (work.legacy_refactor) ", helping others " (work.help_others)
                                                ", churn " (work.churn)
                                            }
                                            ItemKind::Review { number, state } => { "#" (number) " " (state.to_lowercase().replace('_', " ")) }
                                            ItemKind::PullRequestOpened { number } | ItemKind::PullRequestMerged { number } | ItemKind::Comment { number } => {
                                                a href=(ctx.filters.link("/pr", &[("repository", &item.repository_id.to_string()), ("number", &number.to_string())])) { "#" (number) }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                },
            }
        }
    };
    ctx.page(
        &store,
        "Work log day",
        "/work-log/day",
        &[("id", id.clone()), ("day", day.clone())],
        body,
    )
}

pub async fn reviews(State(state): S, Query(filters): Query<Filters>) -> Markup {
    let ctx = Ctx::new(state, filters);
    let store = ctx.state.store.lock().expect("store lock");
    let body = match store.review_summary(&ctx.selection) {
        Err(e) => error(e),
        Ok(summary) => {
            let names = names(&store);
            let authors_link = |id: &str| ctx.filters.link("/reviews/prs", &[("author", id)]);
            let reviewer_link = |id: &str| ctx.filters.link("/reviews/prs", &[("reviewer", id)]);
            let mut matrix: BTreeMap<(&str, &str), usize> = BTreeMap::new();
            for cell in &summary.matrix {
                matrix.insert(
                    (&cell.reviewer_id, &cell.author_id),
                    cell.pull_requests.len(),
                );
            }
            let mut reviewers: Vec<&str> = summary
                .matrix
                .iter()
                .map(|c| c.reviewer_id.as_str())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let mut authors: Vec<&str> = summary
                .matrix
                .iter()
                .map(|c| c.author_id.as_str())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            reviewers.sort_by_key(|id| name_of(&names, id).to_lowercase());
            authors.sort_by_key(|id| name_of(&names, id).to_lowercase());
            html! {
                p.note { "Pull requests merged or closed in the period. Times are calendar hours: median · 75th percentile · number of PRs." }
                h2 { "Authors" }
                div.scroll { table {
                    thead { tr { th { "Author" } th.num { "PRs" } th.num { "Merged" } th.num { "Unreviewed" } th.num { "Cycle time" }
                                 th.num { "Coding" } th.num { "Pickup" } th.num { "Review" } th.num { "Responsiveness" } th.num { "Iterations" } } }
                    tbody {
                        @for a in &summary.authors {
                            @let l = authors_link(&a.developer_id);
                            tr {
                                td { a href=(ctx.filters.link("/developer", &[("id", &a.developer_id)])) { (a.name) } }
                                td.num { (metric(l.clone(), a.pull_requests.to_string())) }
                                td.num { (metric(l.clone(), a.merged.to_string())) }
                                td.num { (metric(l.clone(), match a.unreviewed_share() { Some(s) => format!("{} ({})", a.unreviewed, pct(s)), None => a.unreviewed.to_string() })) }
                                td.num { (metric(l.clone(), agg(a.cycle))) }
                                td.num { (metric(l.clone(), agg(a.coding))) }
                                td.num { (metric(l.clone(), agg(a.pickup))) }
                                td.num { (metric(l.clone(), agg(a.review))) }
                                td.num { (metric(l.clone(), agg(a.responsiveness))) }
                                td.num { (metric(l, a.iterations.to_string())) }
                            }
                        }
                    }
                } }
                h2 { "Reviewers" }
                table {
                    thead { tr { th { "Reviewer" } th.num { "PRs reviewed" } th.num { "Reaction time" } } }
                    tbody {
                        @for r in &summary.reviewers {
                            @let l = reviewer_link(&r.developer_id);
                            tr {
                                td { (r.name) }
                                td.num { (metric(l.clone(), r.pull_requests_reviewed.to_string())) }
                                td.num { (metric(l, agg(r.reaction))) }
                            }
                        }
                    }
                }
                h2 { "Who reviews whom" }
                p.note { "Rows are reviewers, columns are authors; each cell counts the PRs reviewed." }
                div.scroll { table.grid {
                    thead { tr { th {} @for author in &authors { th { (name_of(&names, author)) } } } }
                    tbody {
                        @for reviewer in &reviewers {
                            tr {
                                th { (name_of(&names, reviewer)) }
                                @for author in &authors {
                                    td.num {
                                        @if let Some(n) = matrix.get(&(*reviewer, *author)) {
                                            (metric(ctx.filters.link("/reviews/prs", &[("reviewer", reviewer), ("author", author)]), n.to_string()))
                                        }
                                    }
                                }
                            }
                        }
                    }
                } }
            }
        }
    };
    ctx.page(&store, "Review and collaboration", "/reviews", &[], body)
}

pub async fn review_prs(
    State(state): S,
    Query(filters): Query<Filters>,
    Query(extra): Query<Extra>,
) -> Markup {
    let ctx = Ctx::new(state, filters);
    let store = ctx.state.store.lock().expect("store lock");
    let names = names(&store);
    let body = match store.pr_list(
        &ctx.selection,
        extra.author.as_deref(),
        extra.reviewer.as_deref(),
    ) {
        Err(e) => error(e),
        Ok(prs) => html! {
            p {
                @if let Some(a) = &extra.author { "Authored by " strong { (name_of(&names, a)) } ". " }
                @if let Some(r) = &extra.reviewer { "Reviewed by " strong { (name_of(&names, r)) } ". " }
                (prs.len()) " pull requests."
            }
            div.scroll { table {
                thead { tr { th { "Closed" } th { "Pull request" } th { "Author" } th.num { "Coding" } th.num { "Pickup" }
                             th.num { "Review" } th.num { "Cycle" } th.num { "Iterations" } th { "Reviewed" } } }
                tbody {
                    @for p in &prs {
                        tr {
                            td { (p.day) }
                            td { a href=(ctx.filters.link("/pr", &[("repository", &p.repository_id.to_string()), ("number", &p.number.to_string())])) { "#" (p.number) " " (p.title) } }
                            td { (name_of(&names, &p.author_id)) }
                            @for s in [p.timings.coding_seconds, p.timings.pickup_seconds, p.timings.review_seconds, p.timings.cycle_seconds] {
                                td.num { (s.map_or("–".into(), |s| hours(s as f64))) }
                            }
                            td.num { (p.timings.iterations) }
                            td { (if p.timings.is_unreviewed { "no" } else { "yes" }) }
                        }
                    }
                }
            } }
        },
    };
    let keep = [
        ("author", extra.author.clone().unwrap_or_default()),
        ("reviewer", extra.reviewer.clone().unwrap_or_default()),
    ];
    let keep: Vec<(&str, String)> = keep.into_iter().filter(|(_, v)| !v.is_empty()).collect();
    ctx.page(&store, "Pull requests", "/reviews/prs", &keep, body)
}

pub async fn pr_detail(
    State(state): S,
    Query(filters): Query<Filters>,
    Query(extra): Query<Extra>,
) -> Markup {
    let ctx = Ctx::new(state, filters);
    let store = ctx.state.store.lock().expect("store lock");
    let (repository, number) = (
        extra.repository.unwrap_or_default(),
        extra.number.unwrap_or_default(),
    );
    let tz = ctx.state.config.timezone();
    let fmt = |t: Option<jiff::Timestamp>| {
        t.map_or("–".into(), |t| {
            t.to_zoned(tz.clone())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
    };
    let body = match store.pr_detail(repository, number) {
        Err(e) => error(e),
        Ok(None) => html! { p { "Pull request not found." } },
        Ok(Some(pr)) => html! {
            h2 { a href=(pr.url) { "#" (pr.number) " " (pr.title) } }
            p { "Author: " (pr.author) }
            table { tbody {
                tr { th { "First commit" } td { (fmt(pr.timings.first_commit_at)) } }
                tr { th { "Ready for review" } td { (fmt(pr.timings.ready_at)) } }
                tr { th { "First response" } td { (fmt(pr.timings.first_response_at)) } }
                tr { th { "Merged" } td { (fmt(pr.timings.merged_at)) } }
                @for (label, s) in [("Coding", pr.timings.coding_seconds), ("Pickup", pr.timings.pickup_seconds),
                                    ("Review", pr.timings.review_seconds), ("Cycle time", pr.timings.cycle_seconds)] {
                    tr { th { (label) } td.num { (s.map_or("–".into(), |s| hours(s as f64))) } }
                }
                tr { th { "Iterations" } td.num { (pr.timings.iterations) } }
                tr { th { "Unreviewed" } td { (if pr.timings.is_unreviewed { "yes" } else { "no" }) } }
            } }
            h3 { "Events used" }
            table {
                thead { tr { th { "Time" } th { "Event" } th { "By" } th { "Detail" } } }
                tbody {
                    @for e in &pr.events {
                        tr { td { (fmt(Some(e.at))) } td { (e.kind.replace('_', " ")) } td { (e.actor.clone().unwrap_or_default()) }
                             td { (e.detail.clone().unwrap_or_default()) } }
                    }
                }
            }
        },
    };
    let keep = [
        ("repository", repository.to_string()),
        ("number", number.to_string()),
    ];
    ctx.page(&store, "Pull request", "/pr", &keep, body)
}

pub async fn identities(State(state): S, Query(filters): Query<Filters>) -> Markup {
    let ctx = Ctx::new(state, filters);
    let store = ctx.state.store.lock().expect("store lock");
    let body = match identities::audit(&store) {
        Err(e) => error(e),
        Ok(entries) => html! {
            p.note { "Each raw identity and the developer it resolves to. Fix merges and splits with aliases and logins in kawa.toml." }
            table {
                thead { tr { th { "Identity" } th { "Status" } th { "Developer" } th.num { "Commits" } th.num { "PRs" } th.num { "Reviews" } } }
                tbody {
                    @for e in &entries {
                        tr { td { (e.identity) } td { (e.status) } td { (e.developer.clone().unwrap_or_default()) }
                             td.num { (e.commits) } td.num { (e.pull_requests) } td.num { (e.reviews) } }
                    }
                }
            }
        },
    };
    ctx.page(&store, "Identities", "/identities", &[], body)
}

pub async fn sync_status(State(state): S, Query(filters): Query<Filters>) -> Markup {
    let ctx = Ctx::new(state, filters);
    let store = ctx.state.store.lock().expect("store lock");
    let repositories = store.repositories().unwrap_or_default();
    let body = html! {
        table {
            thead { tr { th { "Repository" } th { "GitHub" } th { "Last git sync" } th { "Head" } th { "Last PR sync" } th { "Last error" } } }
            tbody {
                @for r in &repositories {
                    @let s = store.sync_status(r.id).unwrap_or_default();
                    tr {
                        td { (r.path) }
                        td { (r.github.clone().unwrap_or_else(|| "–".into())) }
                        td { (s.git_synced_at.map_or("never".into(), |t| t.to_string())) }
                        td { code { (s.git_head.clone().map(|h| h.chars().take(10).collect::<String>()).unwrap_or_default()) } }
                        td { (s.pr_synced_at.map_or("never".into(), |t| t.to_string())) }
                        td { @if let Some(e) = &s.error { (e.at.to_string()) ": " (e.message) } }
                    }
                }
            }
        }
    };
    ctx.page(&store, "Sync status", "/sync", &[], body)
}
