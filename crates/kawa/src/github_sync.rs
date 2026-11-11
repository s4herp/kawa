//! Pull request sync from GitHub, read-only and incremental by `updatedAt`.

use std::process::Command;

use jiff::{SignedDuration, Timestamp};
use kawa_github::{self as github, Client, GithubError};
use kawa_store::{Store, StoreError, SyncErrorKind};

use crate::config::Config;

pub const NO_TOKEN_MESSAGE: &str = "no GitHub token: set GITHUB_TOKEN or log in with `gh auth login`; \
     pull request metrics are skipped";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrOutcome {
    Synced {
        path: String,
        pull_requests: u64,
    },
    /// The repository has no `github` name in config.
    NotConfigured {
        path: String,
    },
    Skipped {
        path: String,
        reason: String,
    },
    Failed {
        path: String,
        error: String,
    },
}

/// The token from `GITHUB_TOKEN`, or else from `gh auth token`. Never stored.
pub fn discover_token() -> Option<String> {
    if let Some(token) = std::env::var("GITHUB_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty())
    {
        return Some(token.trim().to_owned());
    }
    let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!token.is_empty()).then_some(token)
}

pub struct GithubSync<'a> {
    pub token: Option<String>,
    pub api_url: String,
    /// Called with the reset time when the rate limit is exhausted; returns
    /// whether to continue after waiting.
    pub wait: Box<dyn FnMut(Timestamp) -> bool + 'a>,
}

/// Sleeps until the reset time, unless `no_wait`.
pub fn wait_policy(no_wait: bool) -> Box<dyn FnMut(Timestamp) -> bool> {
    Box::new(move |reset_at: Timestamp| {
        if no_wait {
            eprintln!("GitHub rate limit exhausted until {reset_at}; stopping (--no-wait)");
            return false;
        }
        eprintln!("GitHub rate limit exhausted; waiting until {reset_at}");
        let left = reset_at.duration_since(Timestamp::now());
        if let Ok(duration) = std::time::Duration::try_from(left + SignedDuration::from_secs(1)) {
            std::thread::sleep(duration);
        }
        true
    })
}

pub fn sync_pull_requests(
    store: &mut Store,
    config: &Config,
    now: Timestamp,
    options: &mut GithubSync<'_>,
) -> Result<Vec<PrOutcome>, StoreError> {
    let mut outcomes = Vec::new();
    let client = match &options.token {
        Some(token) => match Client::new(token.clone(), &options.api_url) {
            Ok(client) => Some(client),
            Err(error) => {
                for repository in &config.repositories {
                    outcomes.push(PrOutcome::Failed {
                        path: repository.path.display().to_string(),
                        error: error.to_string(),
                    });
                }
                return Ok(outcomes);
            }
        },
        None => None,
    };
    for repository in &config.repositories {
        let path = repository.path.display().to_string();
        let Some(full_name) = &repository.github else {
            outcomes.push(PrOutcome::NotConfigured { path });
            continue;
        };
        let Some(client) = &client else {
            outcomes.push(PrOutcome::Skipped {
                path,
                reason: NO_TOKEN_MESSAGE.to_owned(),
            });
            continue;
        };
        let (owner, name) = full_name
            .split_once('/')
            .expect("config validates owner/name");
        let repository_id = store.upsert_repository(
            &path,
            repository.branch.as_deref(),
            repository.github.as_deref(),
        )?;
        let status = store.sync_status(repository_id)?;
        let window_start =
            now - SignedDuration::from_hours(24 * i64::from(config.sync_window_days));
        // Pull requests older than the stored window were never fetched, so a
        // longer window starts over instead of continuing from the last sync.
        let covered_from = status
            .pr_window_start
            .filter(|start| *start <= window_start);
        let previous = status.pr_high_water_mark.filter(|_| covered_from.is_some());
        let since = previous.unwrap_or(window_start);

        let mut stored = 0u64;
        let mut high_water = previous.unwrap_or(window_start);
        let mut store_error: Option<StoreError> = None;
        let result = github::fetch_updated_since(
            client,
            owner,
            name,
            since,
            |page| {
                if let Err(error) = store.replace_pull_requests(repository_id, page) {
                    store_error = Some(error);
                    return Err(GithubError::Decode("storing pull requests failed".into()));
                }
                stored += page.len() as u64;
                if let Some(latest) = page.iter().map(|p| p.updated_at).max() {
                    high_water = high_water.max(latest);
                }
                Ok(())
            },
            &mut options.wait,
        );
        if let Some(error) = store_error {
            return Err(error);
        }
        match result {
            Ok(()) => {
                store.record_pr_sync(
                    repository_id,
                    now,
                    high_water,
                    covered_from.unwrap_or(window_start),
                )?;
                outcomes.push(PrOutcome::Synced {
                    path,
                    pull_requests: stored,
                });
            }
            Err(error) => {
                let message = error.to_string();
                store.record_sync_error(repository_id, SyncErrorKind::Github, now, &message)?;
                outcomes.push(PrOutcome::Failed {
                    path,
                    error: message,
                });
            }
        }
    }
    Ok(outcomes)
}
