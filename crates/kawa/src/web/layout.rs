//! Page shell: navigation, shared filters, staleness notice and styles.

use maud::{DOCTYPE, Markup, PreEscaped, html};

use kawa_store::{Selection, Store};

use super::{Filters, stale_repositories};

const STYLE: &str = r#"
:root { --bg: #ffffff; --fg: #1d2330; --muted: #5f6b7a; --line: #d9dee5; --soft: #f3f5f8;
        --accent: #2f6fb3; --warn-bg: #fff4d6; --warn-fg: #6b4e00; --weekend: #f7f7fa; }
@media (prefers-color-scheme: dark) {
  :root { --bg: #14171c; --fg: #e3e7ee; --muted: #9aa5b4; --line: #2c323c; --soft: #1c2027;
          --accent: #7fb0e8; --warn-bg: #3a2f10; --warn-fg: #f3d58a; --weekend: #191c22; }
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg);
       font: 14px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif; }
header { display: flex; gap: 20px; align-items: baseline; padding: 12px 20px; border-bottom: 1px solid var(--line); }
header strong { font-size: 16px; }
nav a { margin-right: 14px; color: var(--muted); text-decoration: none; }
nav a.active { color: var(--fg); font-weight: 600; }
main { padding: 16px 20px; max-width: 1400px; }
a { color: var(--accent); }
form.filters { display: flex; flex-wrap: wrap; gap: 12px; align-items: end; padding: 10px 12px;
               background: var(--soft); border-radius: 6px; margin-bottom: 14px; }
form.filters label { display: flex; flex-direction: column; font-size: 12px; color: var(--muted); }
select, input, button { font: inherit; color: inherit; background: var(--bg); border: 1px solid var(--line);
                        border-radius: 4px; padding: 3px 6px; }
.notice { background: var(--warn-bg); color: var(--warn-fg); padding: 8px 12px; border-radius: 6px; margin-bottom: 12px; }
.note { color: var(--muted); font-size: 13px; }
table { border-collapse: collapse; margin: 8px 0 20px; }
th, td { border-bottom: 1px solid var(--line); padding: 5px 10px; text-align: left; vertical-align: top; }
th { font-size: 12px; color: var(--muted); font-weight: 600; }
td.num, th.num { text-align: right; font-variant-numeric: tabular-nums; }
.scroll { overflow-x: auto; }
table.grid td, table.grid th { padding: 3px 5px; font-size: 12px; white-space: nowrap; }
table.grid .weekend { background: var(--weekend); }
svg text { fill: var(--fg); font-size: 11px; }
svg .axis { stroke: var(--line); }
svg .median { stroke: var(--muted); stroke-dasharray: 4 4; }
svg .quadrant { fill: var(--muted); font-size: 12px; }
svg circle { fill: var(--accent); }
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
                        label { "Repositories"
                            select name="repo" multiple size="3" {
                                @for repository in &repositories {
                                    option value=(repository.id) selected[page.selection.repositories.contains(&repository.id)] {
                                        (repository.github.clone().unwrap_or_else(|| repository.path.clone()))
                                    }
                                }
                            }
                        }
                        label { "Developers"
                            select name="dev" multiple size="3" {
                                @for (id, name) in &developers {
                                    option value=(id) selected[page.selection.developers.contains(id)] { (name) }
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
