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

use super::charts::{self, Point, STAGES, Segment, WORK};
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

/// Whole number with thousands separators.
fn num(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn tile(label: &str, value: Markup) -> Markup {
    html! { div.tile { div.tile-label { (label) } div.tile-value { (value) } } }
}

fn work_values(w: &WorkTypes) -> [u64; 4] {
    [w.new_work, w.legacy_refactor, w.help_others, w.churn]
}

fn work_segments(w: &WorkTypes) -> Vec<Segment> {
    work_values(w)
        .iter()
        .zip(WORK)
        .map(|(value, (label, class))| Segment {
            label,
            class,
            value: *value as f64,
            text: format!("{} lines", num(*value)),
        })
        .collect()
}

fn sum_work<'a>(items: impl IntoIterator<Item = &'a WorkTypes>) -> WorkTypes {
    items.into_iter().fold(WorkTypes::default(), |mut t, w| {
        t.new_work += w.new_work;
        t.legacy_refactor += w.legacy_refactor;
        t.help_others += w.help_others;
        t.churn += w.churn;
        t
    })
}

/// The work mix bar with its legend, each entry carrying its share.
fn work_mix(w: &WorkTypes) -> Markup {
    let shares = w.shares().unwrap_or_default();
    let labels: Vec<String> = WORK
        .iter()
        .zip(shares)
        .map(|((label, _), share)| format!("{label} {}", pct(share)))
        .collect();
    let items: Vec<(&str, &str)> = labels
        .iter()
        .zip(WORK)
        .map(|(label, (_, class))| (label.as_str(), class))
        .collect();
    html! {
        div.tall { (charts::stack(&work_segments(w))) }
        (charts::legend(&items))
    }
}

fn stage_segments(stages: [Option<Aggregate>; 3]) -> Vec<Segment> {
    stages
        .iter()
        .zip(STAGES)
        .map(|(stage, (label, class))| Segment {
            label,
            class,
            value: stage.map_or(0.0, |a| a.median),
            text: stage.map_or("–".into(), |a| duration(a.median)),
        })
        .collect()
}

fn pct(value: f64) -> String {
    format!("{:.0}%", value * 100.0)
}

fn hours(seconds: f64) -> String {
    format!("{:.1} h", seconds / 3600.0)
}

fn duration(seconds: f64) -> String {
    let minutes = seconds / 60.0;
    if minutes < 60.0 {
        format!("{minutes:.0} min")
    } else if minutes < 48.0 * 60.0 {
        format!("{:.1} h", minutes / 60.0)
    } else {
        format!("{:.1} d", minutes / 1440.0)
    }
}

/// Median as the headline, 75th percentile and sample size underneath.
fn metric_agg(href: String, value: Option<Aggregate>) -> Markup {
    match value {
        Some(a) => html! {
            a.metric href=(href) { (duration(a.median)) }
            span.sub { "p75 " (duration(a.p75)) " · n " (a.n) }
        },
        None => html! { a.metric href=(href) { "–" } },
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
            let total = sum_work(rows.iter().map(|r| &r.work));
            let commits: u64 = rows.iter().map(|r| r.commits).sum();
            html! {
                @if rows.is_empty() {
                    p { "No commits in this period." }
                } @else {
                    div.tiles {
                        (tile("Developers", html! { (rows.len()) }))
                        (tile("Commits", html! { (num(commits)) }))
                        (tile("Changed lines", html! { (num(total.changed())) }))
                        (tile("Efficiency", html! { (total.efficiency().map_or("–".into(), pct)) }))
                    }
                    div.card {
                        h2 { "Work mix" }
                        (work_mix(&total))
                    }
                    div.card {
                        h2 { "Impact and efficiency" }
                        (charts::quadrant(&points))
                        p.note {
                            "Each point is a developer's pattern of activity in this selection, split at the medians. "
                            "This chart is not a performance evaluation: design, incidents, reviews and helping others "
                            "often leave few lines behind."
                        }
                        @if !insufficient.is_empty() {
                            p.note { "Not plotted, insufficient data (fewer than " (settings.min_lines) " changed lines): " (insufficient.join(", ")) }
                        }
                    }
                    div.scroll { table {
                        thead { tr {
                            th { "Developer" } th { "Work mix" } th.num { "Active days" } th.num { "Commits per active day" }
                            th.num { "New work" } th.num { "Legacy refactor" } th.num { "Helping others" }
                            th.num { "Churn" } th.num { "Efficiency" } th.num { "Impact" }
                        } }
                        tbody {
                            @for r in &rows {
                                tr.dim[r.impact.is_none()] {
                                    td { a href=(ctx.filters.link("/developer", &[("id", &r.developer_id)])) { (r.name) } }
                                    td { (charts::stack(&work_segments(&r.work))) }
                                    td.num { (metric(link(&r.developer_id, "commits"), r.active_days.to_string())) }
                                    td.num { (metric(link(&r.developer_id, "commits"), format!("{:.1}", r.commits_per_active_day))) }
                                    td.num { (metric(link(&r.developer_id, "new_work"), num(r.work.new_work))) }
                                    td.num { (metric(link(&r.developer_id, "legacy_refactor"), num(r.work.legacy_refactor))) }
                                    td.num { (metric(link(&r.developer_id, "help_others"), num(r.work.help_others))) }
                                    td.num { (metric(link(&r.developer_id, "churn"), num(r.work.churn))) }
                                    td.num { (metric(link(&r.developer_id, "churn"), r.efficiency.map_or("–".into(), pct))) }
                                    td.num { (metric(link(&r.developer_id, "impact"), r.impact.map_or("–".into(), |v| format!("{v:.2}")))) }
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
            let total = sum_work(commits.iter().map(|c| &c.work));
            let github: HashMap<i64, String> = store
                .repositories()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|r| r.github.map(|g| (r.id, g)))
                .collect();
            html! {
                p { "Commits by " strong { (name_of(&names, &id)) } " in this selection"
                    @if let Some(m) = &extra.metric { " (showing evidence for " (m.replace('_', " ")) ")" } "." }
                div.card { (work_mix(&total)) }
                div.scroll { table {
                    thead { tr { th { "Day" } th { "Commit" } th.num { "New work" } th.num { "Legacy refactor" }
                                 th.num { "Helping others" } th.num { "Churn" } th { "Rewrote lines from" } } }
                    tbody {
                        @for c in &commits {
                            tr {
                                td.nowrap { (c.day) }
                                td.wide {
                                    @if let Some(g) = github.get(&c.repository_id) {
                                        a href=(format!("https://github.com/{g}/commit/{}", c.sha)) { code { (&c.sha[..10]) } }
                                    } @else { code { (&c.sha[..10]) } }
                                    " " (c.subject)
                                }
                                td.num { (num(c.work.new_work)) } td.num { (num(c.work.legacy_refactor)) }
                                td.num { (num(c.work.help_others)) } td.num { (num(c.work.churn)) }
                                td.rewrote {
                                    @for r in &c.rewrote {
                                        @let (kind, class) = match r.work_type {
                                            WorkType::NewWork => ("new work", WORK[0].1),
                                            WorkType::LegacyRefactor => ("legacy refactor", WORK[1].1),
                                            WorkType::HelpOthers => ("helping others", WORK[2].1),
                                            WorkType::Churn => ("churn", WORK[3].1),
                                        };
                                        div {
                                            i.dot class=(class) {}
                                            (r.lines) " lines · " (kind) " · from "
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
                        tr.total { th { "Total" } th {} th.num { (num(total.new_work)) } th.num { (num(total.legacy_refactor)) }
                             th.num { (num(total.help_others)) } th.num { (num(total.churn)) } th {} }
                    }
                } }
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
    let bars: Vec<(String, [u64; 4])> = weeks
        .iter()
        .map(|(week, w)| (week.strftime("%b %d").to_string(), work_values(w)))
        .collect();
    let body = html! {
        h2 { (name_of(&names, &id)) }
        @if let Some(r) = &fundamentals {
            div.tiles {
                (tile("Active days", metric(link("commits"), r.active_days.to_string())))
                (tile("Commits", metric(link("commits"), num(r.commits))))
                (tile("Changed lines", metric(link("commits"), num(r.work.changed()))))
                (tile("Efficiency", metric(link("churn"), r.efficiency.map_or("–".into(), pct))))
                (tile("Impact", metric(link("impact"), r.impact.map_or("–".into(), |v| format!("{v:.2}")))))
            }
            div.card {
                h2 { "Work mix" }
                (work_mix(&r.work))
            }
        } @else {
            p { "No commits in this period." }
        }
        @if !weeks.is_empty() {
            div.card {
                h2 { "Changed lines per week" }
                (charts::stacked_bars(&bars))
                (charts::legend(&WORK))
            }
        }
        h2 { "Work types per week" }
        table {
            thead { tr { th { "Week of" } th.num { "New work" } th.num { "Legacy refactor" } th.num { "Helping others" } th.num { "Churn" } } }
            tbody {
                @for (week, w) in &weeks {
                    tr {
                        td { (week.to_string()) }
                        @for (value, m) in [(w.new_work, "new_work"), (w.legacy_refactor, "legacy_refactor"), (w.help_others, "help_others"), (w.churn, "churn")] {
                            td.num { (metric(link(m), num(value))) }
                        }
                    }
                }
            }
        }
        h2 { "Pull requests" }
        @if let Some(a) = as_author {
            @let prs = ctx.filters.link("/reviews/prs", &[("author", &id)]);
            div.tiles {
                (tile("Merged", metric(prs.clone(), a.merged.to_string())))
                (tile("Cycle time", metric_agg(prs.clone(), a.cycle)))
                (tile("Pickup", metric_agg(prs.clone(), a.pickup)))
                (tile("Responsiveness", metric_agg(prs.clone(), a.responsiveness)))
                (tile("Unreviewed", metric(prs, a.unreviewed.to_string())))
            }
        } @else { p { "No pull requests merged or closed in this period." } }
        @if let Some(r) = as_reviewer {
            @let prs = ctx.filters.link("/reviews/prs", &[("reviewer", &id)]);
            h2 { "Reviews given" }
            div.tiles {
                (tile("PRs reviewed", metric(prs.clone(), r.pull_requests_reviewed.to_string())))
                (tile("Reaction time", metric_agg(prs, r.reaction)))
            }
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
                    (charts::legend(&[("C commits", "s1"), ("O pull requests opened", "s2"), ("M merged", "s3"), ("R reviews", "s4"), ("K comments", "s5")]))
                    p.note { "Empty days carry no meaning." }
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
                                                    a.chips href=(ctx.filters.link("/work-log/day", &[("id", id), ("day", &key)])) {
                                                        @for (letter, class, n) in [("C", "s1", a.commits), ("O", "s2", a.prs_opened), ("M", "s3", a.prs_merged), ("R", "s4", a.reviews), ("K", "s5", a.comments)] {
                                                            @if n > 0 { span.chip { i class=(class) {} (letter) (n) } }
                                                        }
                                                    }
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
            let peak = matrix.values().copied().max().unwrap_or(1).max(1);
            let (prs, merged, unreviewed) = summary.authors.iter().fold((0, 0, 0), |t, a| {
                (t.0 + a.pull_requests, t.1 + a.merged, t.2 + a.unreviewed)
            });
            html! {
                div.tiles {
                    (tile("Pull requests", html! { (num(prs)) }))
                    (tile("Merged", html! { (num(merged)) }))
                    (tile("Merged unreviewed", html! { (num(unreviewed)) }))
                    (tile("Authors", html! { (summary.authors.len()) }))
                    (tile("Reviewers", html! { (summary.reviewers.len()) }))
                }
                p.note { "Pull requests merged or closed in the period. Times are calendar time: the median on top, then the 75th percentile and the number of PRs." }
                h2 { "Authors" }
                (charts::legend(&STAGES))
                div.scroll { table {
                    thead { tr { th { "Author" } th.num { "PRs" } th.num { "Merged" } th.num { "Unreviewed" } th { "Stages (median)" } th.num { "Cycle time" }
                                 th.num { "Coding" } th.num { "Pickup" } th.num { "Review" } th.num { "Responsiveness" } th.num { "Iterations" } } }
                    tbody {
                        @for a in &summary.authors {
                            @let l = authors_link(&a.developer_id);
                            tr {
                                td { a href=(ctx.filters.link("/developer", &[("id", &a.developer_id)])) { (a.name) } }
                                td.num { (metric(l.clone(), a.pull_requests.to_string())) }
                                td.num { (metric(l.clone(), a.merged.to_string())) }
                                td.num { (metric(l.clone(), match a.unreviewed_share() { Some(s) => format!("{} ({})", a.unreviewed, pct(s)), None => a.unreviewed.to_string() })) }
                                td { (charts::stack(&stage_segments([a.coding, a.pickup, a.review]))) }
                                td.num { (metric_agg(l.clone(), a.cycle)) }
                                td.num { (metric_agg(l.clone(), a.coding)) }
                                td.num { (metric_agg(l.clone(), a.pickup)) }
                                td.num { (metric_agg(l.clone(), a.review)) }
                                td.num { (metric_agg(l.clone(), a.responsiveness)) }
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
                                td.num { (metric_agg(l, r.reaction)) }
                            }
                        }
                    }
                }
                h2 { "Who reviews whom" }
                p.note { "Rows are reviewers, columns are authors; each cell counts the PRs reviewed." }
                div.scroll { table.grid.matrix {
                    thead { tr { th {} @for author in &authors { th.col { (name_of(&names, author)) } } } }
                    tbody {
                        @for reviewer in &reviewers {
                            tr {
                                th { (name_of(&names, reviewer)) }
                                @for author in &authors {
                                    @let n = matrix.get(&(*reviewer, *author));
                                    td.num style=[n.map(|n| format!("background: color-mix(in srgb, var(--s1) {}%, transparent)", 8 + n * 52 / peak))] {
                                        @if let Some(n) = n {
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
            h2 { "Events used" }
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
