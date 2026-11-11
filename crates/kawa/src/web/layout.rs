//! Page shell: navigation, shared filters, staleness notice and styles.

use maud::{DOCTYPE, Markup, PreEscaped, html};

use kawa_store::{Selection, Store};

use super::{Filters, stale_repositories};

const STYLE: &str = r#"
:root { --bg: #f6f7f9; --card: #ffffff; --fg: #1d2330; --muted: #667085; --line: #e3e6eb; --soft: #f0f2f5;
        --accent: #3b6fd8; --accent-soft: rgba(59,111,216,.10); --warn-bg: #fff4d6; --warn-fg: #6b4e00;
        --weekend: #f3f4f7; --zone: rgba(59,111,216,.045); --shadow: 0 1px 2px rgba(16,24,40,.06); }
@media (prefers-color-scheme: dark) {
  :root { --bg: #0f1216; --card: #171b21; --fg: #e6e9ef; --muted: #8b95a5; --line: #262c35; --soft: #1d222a;
          --accent: #7aa7f0; --accent-soft: rgba(122,167,240,.14); --warn-bg: #3a2f10; --warn-fg: #f3d58a;
          --weekend: #1a1e25; --zone: rgba(122,167,240,.05); --shadow: none; }
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg);
       font: 14px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif; -webkit-font-smoothing: antialiased; }
header { display: flex; gap: 28px; align-items: center; padding: 0 28px; height: 52px;
         background: var(--card); border-bottom: 1px solid var(--line); position: sticky; top: 0; z-index: 20; }
header strong { font-size: 17px; letter-spacing: .2px; }
nav { display: flex; gap: 4px; }
nav a { padding: 6px 12px; border-radius: 6px; color: var(--muted); text-decoration: none; font-weight: 500; }
nav a:hover { color: var(--fg); background: var(--soft); }
nav a.active { color: var(--accent); background: var(--accent-soft); }
main { padding: 24px 28px 48px; max-width: 1400px; margin: 0 auto; }
h1 { font-size: 22px; font-weight: 650; margin: 8px 0 16px; }
h2 { font-size: 16px; font-weight: 600; margin: 24px 0 8px; }
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
.card { background: var(--card); border: 1px solid var(--line); border-radius: 10px; box-shadow: var(--shadow);
        padding: 16px 20px; margin-bottom: 18px; }
form.filters { display: flex; flex-wrap: wrap; gap: 14px; align-items: flex-end; padding: 12px 16px;
               background: var(--card); border: 1px solid var(--line); border-radius: 10px;
               box-shadow: var(--shadow); margin-bottom: 20px; }
form.filters label, form.filters .field { display: flex; flex-direction: column; gap: 4px;
                                          font-size: 12px; color: var(--muted); font-weight: 500; }
select, input, button, summary.control { font: inherit; color: var(--fg); background: var(--card);
                        border: 1px solid var(--line); border-radius: 6px; padding: 5px 9px; height: 32px; }
button { background: var(--accent); border-color: var(--accent); color: #fff; font-weight: 600; padding: 5px 16px; cursor: pointer; }
details.pick { position: relative; }
details.pick > summary { list-style: none; cursor: pointer; min-width: 220px; display: flex; align-items: center;
                         justify-content: space-between; gap: 10px; white-space: nowrap; }
details.pick > summary::-webkit-details-marker { display: none; }
details.pick > summary::after { content: "\25BE"; color: var(--muted); }
details.pick[open] > summary { border-color: var(--accent); }
details.pick .menu { position: absolute; top: 36px; left: 0; z-index: 30; min-width: 100%; max-height: 340px;
                     overflow-y: auto; background: var(--card); border: 1px solid var(--line); border-radius: 8px;
                     box-shadow: 0 8px 24px rgba(16,24,40,.18); padding: 6px; }
details.pick .menu label { flex-direction: row; align-items: center; gap: 8px; padding: 5px 8px; border-radius: 5px;
                           color: var(--fg); font-size: 13px; font-weight: 400; white-space: nowrap; cursor: pointer; }
details.pick .menu label:hover { background: var(--soft); }
details.pick .menu input { height: auto; accent-color: var(--accent); }
.notice { background: var(--warn-bg); color: var(--warn-fg); padding: 10px 14px; border-radius: 8px; margin-bottom: 16px; }
.notice ul { margin: 4px 0 0; padding-left: 18px; }
.note { color: var(--muted); font-size: 13px; }
.muted { color: var(--muted); }
.sub { display: block; color: var(--muted); font-size: 11px; white-space: nowrap; }
table { border-collapse: separate; border-spacing: 0; width: 100%; margin: 8px 0 20px; background: var(--card);
        border: 1px solid var(--line); border-radius: 10px; overflow: hidden; box-shadow: var(--shadow); }
th, td { border-bottom: 1px solid var(--line); padding: 9px 10px; text-align: left; vertical-align: top; }
tbody tr:last-child td { border-bottom: none; }
tbody tr:hover td { background: var(--soft); }
th { font-size: 11px; color: var(--muted); font-weight: 600; text-transform: uppercase; letter-spacing: .4px;
     background: var(--soft); vertical-align: bottom; }
td.num, th.num { text-align: right; font-variant-numeric: tabular-nums; }
a.metric { color: var(--fg); }
a.metric:hover { color: var(--accent); }
.scroll { overflow-x: auto; }
table.grid { width: auto; }
table.grid td, table.grid th { padding: 4px 6px; font-size: 12px; white-space: nowrap; }
table.grid .weekend { background: var(--weekend); }
svg { max-width: 100%; height: auto; }
svg.quadrant-chart { display: block; width: 100%; }
svg text { fill: var(--fg); font-size: 11px; }
svg .plot { fill: none; stroke: var(--line); }
svg .zone { fill: var(--zone); }
svg .grid { stroke: var(--line); stroke-width: 1; }
svg .tick { fill: var(--muted); font-size: 11px; }
svg .axis-title { fill: var(--muted); font-size: 12px; font-weight: 500; }
svg .median { stroke: var(--muted); stroke-dasharray: 4 4; opacity: .7; }
svg .corner { fill: var(--muted); font-size: 11px; font-weight: 500; text-transform: uppercase; letter-spacing: .4px; opacity: .8; }
svg circle { fill: var(--accent); stroke: var(--card); stroke-width: 2; }
svg a.dot:hover circle { r: 7; }
svg a.dot text.label { font-size: 11.5px; }
svg a.dot:hover text.label { font-weight: 700; }
svg text.hover { display: none; paint-order: stroke; stroke: var(--card); stroke-width: 4px; }
svg a.dot:hover text.hover { display: inline; }
:root { --s1: #2a78d6; --s2: #eb6834; --s3: #1baf7a; --s4: #eda100; --s5: #e87ba4; }
@media (prefers-color-scheme: dark) {
  :root { --s1: #3987e5; --s2: #d95926; --s3: #199e70; --s4: #c98500; --s5: #d55181; }
}
.s1 { background: var(--s1); fill: var(--s1); } .s2 { background: var(--s2); fill: var(--s2); }
.s3 { background: var(--s3); fill: var(--s3); } .s4 { background: var(--s4); fill: var(--s4); }
.s5 { background: var(--s5); fill: var(--s5); }
.tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(170px, 1fr)); gap: 12px; margin-bottom: 18px; }
.tile { background: var(--card); border: 1px solid var(--line); border-radius: 10px; box-shadow: var(--shadow); padding: 14px 16px; }
.tile-label { font-size: 11px; font-weight: 600; text-transform: uppercase; letter-spacing: .4px; color: var(--muted); }
.tile-value { font-size: 24px; font-weight: 650; line-height: 1.3; font-variant-numeric: tabular-nums; }
.tile-value a { color: var(--fg); }
.stack { display: flex; gap: 2px; height: 10px; min-width: 90px; border-radius: 3px; overflow: hidden; }
.stack span { min-width: 2px; flex-basis: 0; border-radius: 2px; }
.tall .stack { height: 18px; }
.nowrap { white-space: nowrap; }
td .stack { margin-top: 6px; }
tbody td:first-child { white-space: nowrap; }
.legend { display: flex; flex-wrap: wrap; gap: 6px 18px; font-size: 12px; color: var(--muted); margin: 10px 0 2px; }
.legend i { display: inline-block; width: 10px; height: 10px; border-radius: 2px; margin-right: 6px; }
.card h2 { margin-top: 0; }
a.chips { display: inline-flex; gap: 3px; text-decoration: none; }
.chip { display: inline-flex; align-items: center; gap: 4px; padding: 0 6px 0 5px; height: 18px; border-radius: 9px;
        font-size: 11px; font-variant-numeric: tabular-nums; color: var(--fg); background: var(--soft); }
.chip i { width: 7px; height: 7px; border-radius: 50%; }
a.chips:hover .chip { outline: 1px solid var(--accent); }
table.grid td:first-child, table.grid th:first-child { position: sticky; left: 0; background: var(--card); z-index: 1; }
table.grid thead th:first-child { background: var(--soft); }
table.matrix th.col { writing-mode: vertical-rl; transform: rotate(180deg); text-align: left; padding: 8px 4px;
                      text-transform: none; letter-spacing: 0; font-size: 12px; font-weight: 500; }
table.matrix tbody th { text-transform: none; letter-spacing: 0; font-size: 12px; font-weight: 500; color: var(--fg); background: var(--card); }
table.matrix td { text-align: center; min-width: 34px; }
.dot { display: inline-block; width: 8px; height: 8px; border-radius: 50%; margin-right: 6px; }
.rewrote div { font-size: 12.5px; color: var(--muted); }
td.wide { min-width: 280px; }
tr.dim td { color: var(--muted); } tr.dim a { color: var(--muted); }
tr.total th { background: var(--soft); font-size: 13px; color: var(--fg); text-transform: none; letter-spacing: 0; }
svg.bars { display: block; width: 100%; }
code { font-size: 12.5px; }
"#;

pub struct Page<'a> {
    pub title: &'a str,
    pub path: &'a str,
    pub filters: &'a Filters,
    pub selection: &'a Selection,
    /// Page-specific query parameters kept when filters change.
    pub keep: &'a [(&'a str, String)],
}

const NAV: &[(&str, &str)] = &[
    ("/", "Fundamentals"),
    ("/work-log", "Work log"),
    ("/reviews", "Reviews"),
    ("/identities", "Identities"),
    ("/sync", "Sync"),
];

/// Text of a filter dropdown: the single choice, a count, or `all`.
fn summary<K: Ord>(
    options: &[(K, String)],
    selected: &std::collections::BTreeSet<K>,
    all: &str,
) -> String {
    let chosen: Vec<&String> = options
        .iter()
        .filter(|(k, _)| selected.contains(k))
        .map(|(_, label)| label)
        .collect();
    match chosen.as_slice() {
        [] => all.to_owned(),
        [one] => (*one).clone(),
        many => format!("{} selected", many.len()),
    }
}

pub fn render(store: &Store, now: jiff::Timestamp, page: &Page<'_>, body: Markup) -> Markup {
    let stale = stale_repositories(store, page.selection, now);
    let repositories = store.repositories().unwrap_or_default();
    let mut developers: Vec<(String, String)> = store
        .developer_names()
        .unwrap_or_default()
        .into_iter()
        .collect();
    developers.sort_by(|a, b| {
        a.1.to_lowercase()
            .cmp(&b.1.to_lowercase())
            .then_with(|| a.0.cmp(&b.0))
    });
    let repository_labels: Vec<(i64, String)> = repositories
        .iter()
        .map(|r| {
            let full = r.github.clone().unwrap_or_else(|| r.path.clone());
            let short = full.rsplit('/').next().unwrap_or(&full).to_owned();
            (r.id, short)
        })
        .collect();
    let period = page.filters.period.clone().unwrap_or_else(|| "30".into());
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (page.title) " · kawa" }
                style { (PreEscaped(STYLE)) }
            }
            body {
                header {
                    strong { "kawa" }
                    nav {
                        @for (path, label) in NAV {
                            a href=(page.filters.link(path, &[])) class=[(page.path == *path).then_some("active")] { (label) }
                        }
                    }
                }
                main {
                    form.filters method="get" action=(page.path) {
                        @for (key, value) in page.keep {
                            input type="hidden" name=(key) value=(value);
                        }
                        label { "Period"
                            select name="period" {
                                @for (value, label) in [("7", "Last 7 days"), ("30", "Last 30 days"), ("90", "Last 90 days"), ("custom", "Custom")] {
                                    option value=(value) selected[period == value] { (label) }
                                }
                            }
                        }
                        label { "From" input type="date" name="start" value=(page.selection.start.to_string()); }
                        label { "To" input type="date" name="end" value=(page.selection.end.to_string()); }
                        div.field { "Repositories"
                            details.pick {
                                summary.control { (summary(&repository_labels, &page.selection.repositories, "All repositories")) }
                                div.menu {
                                    @for (id, label) in &repository_labels {
                                        label {
                                            input type="checkbox" name="repo" value=(id) checked[page.selection.repositories.contains(id)];
                                            (label)
                                        }
                                    }
                                }
                            }
                        }
                        div.field { "Developers"
                            details.pick {
                                summary.control { (summary(&developers, &page.selection.developers, "All developers")) }
                                div.menu {
                                    @for (id, name) in &developers {
                                        label {
                                            input type="checkbox" name="dev" value=(id) checked[page.selection.developers.contains(id)];
                                            (name)
                                        }
                                    }
                                }
                            }
                        }
                        button type="submit" { "Apply" }
                    }
                    @if !stale.is_empty() {
                        div.notice {
                            "Data may be out of date. Run " code { "kawa sync" } " to refresh."
                            ul {
                                @for (path, reason) in &stale {
                                    li { (path) ": " (reason) }
                                }
                            }
                        }
                    }
                    h1 { (page.title) }
                    (body)
                }
            }
        }
    }
}
