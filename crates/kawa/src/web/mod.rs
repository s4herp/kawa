//! Local web dashboard, served by `kawa serve`.

mod charts;
mod layout;
mod pages;

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::get;
use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};
use kawa_store::{Selection, Store};
use serde::Deserialize;

use crate::config::Config;

pub type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

pub struct AppState {
    pub store: Mutex<Store>,
    pub config: Config,
    pub clock: Clock,
}

impl AppState {
    fn today(&self) -> Date {
        (self.clock)().to_zoned(self.config.timezone()).date()
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(pages::overview))
        .route("/evidence", get(pages::evidence))
        .route("/developer", get(pages::developer))
        .route("/work-log", get(pages::work_log))
        .route("/work-log/day", get(pages::work_log_day))
        .route("/reviews", get(pages::reviews))
        .route("/reviews/prs", get(pages::review_prs))
        .route("/pr", get(pages::pr_detail))
        .route("/identities", get(pages::identities))
        .route("/sync", get(pages::sync_status))
        .with_state(state)
}

/// Refuses non-loopback addresses unless explicitly allowed: the dashboard
/// has no authentication.
pub fn check_bind(address: IpAddr, allow_remote: bool) -> Result<(), String> {
    if address.is_loopback() || allow_remote {
        Ok(())
    } else {
        Err(format!(
            "refusing to listen on {address}: the dashboard has no login; pass --allow-remote to expose it"
        ))
    }
}

pub async fn serve(state: Arc<AppState>, address: SocketAddr) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!("kawa dashboard on http://{}", listener.local_addr()?);
    axum::serve(listener, router(state)).await
}

pub const MAX_WORK_LOG_DAYS: i32 = 92;
const STALE_AFTER: SignedDuration = SignedDuration::from_hours(24);

/// Filters shared by every view, kept in the query string.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Filters {
    pub period: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub repo: Vec<i64>,
    pub dev: Vec<String>,
}

impl Filters {
    pub fn selection(&self, today: Date) -> Selection {
        let days = match self.period.as_deref() {
            Some("7") => Some(7),
            Some("90") => Some(90),
            Some("custom") => None,
            _ => Some(30),
        };
        let (start, end) = match days {
            Some(days) => (
                today.saturating_sub(jiff::Span::new().days(days - 1)),
                today,
            ),
            None => {
                let start = self.start.as_deref().and_then(|s| s.parse::<Date>().ok());
                let end = self.end.as_deref().and_then(|s| s.parse::<Date>().ok());
                match (start, end) {
                    (Some(start), Some(end)) if start <= end => (start, end),
                    _ => (today.saturating_sub(jiff::Span::new().days(29)), today),
                }
            }
        };
        Selection {
            start,
            end,
            repositories: self.repo.iter().copied().collect(),
            developers: self.dev.iter().cloned().collect::<BTreeSet<_>>(),
        }
    }

    /// The filters as a query string, without the leading `?`.
    pub fn query(&self) -> String {
        let mut parts = Vec::new();
        if let Some(period) = &self.period {
            parts.push(format!("period={}", encode(period)));
        }
        if self.period.as_deref() == Some("custom") {
            for (key, value) in [("start", &self.start), ("end", &self.end)] {
                if let Some(value) = value {
                    parts.push(format!("{key}={}", encode(value)));
                }
            }
        }
        parts.extend(self.repo.iter().map(|r| format!("repo={r}")));
        parts.extend(self.dev.iter().map(|d| format!("dev={}", encode(d))));
        parts.join("&")
    }

    /// `path?filters&extra`.
    pub fn link(&self, path: &str, extra: &[(&str, &str)]) -> String {
        let mut query = self.query();
        for (key, value) in extra {
            if !query.is_empty() {
                query.push('&');
            }
            query.push_str(&format!("{key}={}", encode(value)));
        }
        if query.is_empty() {
            path.to_owned()
        } else {
            format!("{path}?{query}")
        }
    }
}

pub fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Repositories in the selection whose last git sync failed or is older than a day.
fn stale_repositories(
    store: &Store,
    selection: &Selection,
    now: Timestamp,
) -> Vec<(String, String)> {
    let Ok(repositories) = store.repositories() else {
        return Vec::new();
    };
    let mut stale = Vec::new();
    for repository in repositories {
        if !selection.repositories.is_empty() && !selection.repositories.contains(&repository.id) {
            continue;
        }
        let Ok(status) = store.sync_status(repository.id) else {
            continue;
        };
        if let Some(error) = &status.error {
            stale.push((
                repository.path.clone(),
                format!("last sync failed at {}: {}", error.at, error.message),
            ));
            continue;
        }
        match status.git_synced_at {
            Some(at) if now.duration_since(at) <= STALE_AFTER => {}
            Some(at) => stale.push((repository.path.clone(), format!("last synced at {at}"))),
            None => stale.push((repository.path.clone(), "never synced".to_owned())),
        }
    }
    stale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_loopback_bind_needs_the_flag() {
        assert!(
            check_bind("127.0.0.1".parse().unwrap(), false).is_ok(),
            "should allow loopback"
        );
        assert!(
            check_bind("::1".parse().unwrap(), false).is_ok(),
            "should allow IPv6 loopback"
        );
        assert!(
            check_bind("0.0.0.0".parse().unwrap(), false).is_err(),
            "should refuse all interfaces without --allow-remote"
        );
        assert!(
            check_bind("192.168.1.5".parse().unwrap(), true).is_ok(),
            "should allow other addresses with --allow-remote"
        );
    }

    #[test]
    fn presets_and_custom_periods() {
        let today: Date = "2026-11-10".parse().unwrap();
        let seven = Filters {
            period: Some("7".into()),
            ..Filters::default()
        }
        .selection(today);
        assert_eq!(
            (seven.start.to_string(), seven.end.to_string()),
            ("2026-11-04".into(), "2026-11-10".into()),
            "should cover 7 days ending today"
        );
        let custom = Filters {
            period: Some("custom".into()),
            start: Some("2026-10-01".into()),
            end: Some("2026-10-15".into()),
            ..Filters::default()
        }
        .selection(today);
        assert_eq!(
            custom.start.to_string(),
            "2026-10-01",
            "should use the custom start"
        );
        let default = Filters::default().selection(today);
        assert_eq!(
            default.start.to_string(),
            "2026-10-12",
            "should default to 30 days"
        );
    }

    #[test]
    fn query_round_trips_through_links() {
        let filters = Filters {
            period: Some("90".into()),
            repo: vec![1, 2],
            dev: vec!["gh:luis".into()],
            ..Filters::default()
        };
        assert_eq!(
            filters.link("/work-log", &[("x", "a b")]),
            "/work-log?period=90&repo=1&repo=2&dev=gh%3Aluis&x=a%20b",
            "should keep every filter and encode values"
        );
    }
}
