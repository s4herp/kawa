//! `kawa sync`: replay each configured repository and store the results.

use std::collections::HashSet;

use jiff::{SignedDuration, Timestamp};
use kawa_core::engine::{self, CommitRecord, Introducer, Replay, ReplayOptions};
use kawa_core::{gix, repo};
use kawa_store::{CommitInput, FileInput, RemovalInput, Store, StoreError, SyncErrorKind};
use thiserror::Error;

use crate::config::{Config, Repository};
use crate::identities;

#[derive(Debug, Error)]
pub enum SyncError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Identities(#[from] identities::IdentitiesError),
    #[error("invalid exclude pattern: {0}")]
    Exclude(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositoryOutcome {
    Synced {
        path: String,
        head: String,
        commits: u64,
    },
    Failed {
        path: String,
        error: String,
    },
}

/// Syncs git history for every configured repository. A repository that
/// fails is recorded and the others continue.
pub fn sync_git(
    store: &mut Store,
    config: &Config,
    now: Timestamp,
) -> Result<Vec<RepositoryOutcome>, SyncError> {
    let excluded =
        engine::path_filter(&config.exclude).map_err(|e| SyncError::Exclude(e.to_string()))?;
    let since = boundary(config, now);
    let mut outcomes = Vec::new();
    for repository in &config.repositories {
        let path = repository.path.display().to_string();
        let repository_id = store.upsert_repository(
            &path,
            repository.branch.as_deref(),
            repository.github.as_deref(),
        )?;
        match replay_repository(repository, config, &excluded, since) {
            Ok((head, replay)) => {
                let (commits, removals) = to_store_inputs(&replay);
                store.replace_git_history(repository_id, &commits, &removals)?;
                store.record_git_sync(repository_id, now, &head)?;
                outcomes.push(RepositoryOutcome::Synced {
                    path,
                    head,
                    commits: commits.len() as u64,
                });
            }
            Err(error) => {
                store.record_sync_error(repository_id, SyncErrorKind::Git, now, &error)?;
                outcomes.push(RepositoryOutcome::Failed { path, error });
            }
        }
    }
    Ok(outcomes)
}

/// Re-resolves every stored identity against the current config and mailmaps.
pub fn resolve_identities(store: &mut Store, config: &Config) -> Result<(), SyncError> {
    let rules = identities::rules_from_config(config)?;
    identities::resolve(store, &rules, &identities::developer_specs(config))?;
    Ok(())
}

/// Re-resolves identities and rebuilds derived tables, so config changes
/// apply to all stored history without a new replay.
pub fn refresh(store: &mut Store, config: &Config) -> Result<(), SyncError> {
    resolve_identities(store, config)?;
    store.rebuild_commit_stats(&config.timezone(), config.recency_days)?;
    Ok(())
}

/// Replay starts `sync_window_days + recency_days` before `now`, so lines
/// changed on the first analyzable day already have real origins.
pub fn boundary(config: &Config, now: Timestamp) -> i64 {
    let days = i64::from(config.sync_window_days) + i64::from(config.recency_days);
    (now - SignedDuration::from_hours(24 * days)).as_second()
}

fn replay_repository(
    repository: &Repository,
    config: &Config,
    excluded: &globset::GlobSet,
    since: i64,
) -> Result<(String, Replay), String> {
    let opened = repo::open(&repository.path).map_err(|e| e.to_string())?;
    let tip = repo::branch_tip(&opened, repository.branch.as_deref()).map_err(|e| e.to_string())?;
    let mut ignore_revs: HashSet<gix::ObjectId> =
        repo::ignore_revs_at(&tip).map_err(|e| e.to_string())?;
    ignore_revs.extend(
        config
            .ignore_revs
            .iter()
            .filter_map(|hex| gix::ObjectId::from_hex(hex.as_bytes()).ok()),
    );
    let options = ReplayOptions {
        since,
        excluded: excluded.clone(),
        ignore_revs,
    };
    let replay = engine::replay(&opened, tip.id, &options).map_err(|e| e.to_string())?;
    Ok((tip.id.to_string(), replay))
}

pub fn to_store_inputs(replay: &Replay) -> (Vec<CommitInput>, Vec<RemovalInput>) {
    let commits = replay.commits.iter().map(commit_input).collect();
    let removals = replay
        .removals
        .iter()
        .map(|pair| RemovalInput {
            introduced_sha: match &pair.introduced {
                Introducer::Baseline => None,
                Introducer::Commit(id) => Some(id.to_string()),
            },
            removed_sha: pair.removed.to_string(),
            lines: pair.lines,
        })
        .collect();
    (commits, removals)
}

fn commit_input(record: &CommitRecord) -> CommitInput {
    CommitInput {
        sha: record.id.to_string(),
        author_name: record.author_name.clone(),
        author_email: record.author_email.clone(),
        author_at: Timestamp::from_second(record.author_time).unwrap_or(Timestamp::UNIX_EPOCH),
        author_offset_seconds: record.author_offset,
        subject: record.subject.clone(),
        is_merge: record.is_merge,
        is_ignored: record.is_ignored,
        files: record
            .files
            .iter()
            .map(|f| FileInput {
                path: f.path.clone(),
                old_path: f.old_path.clone(),
                lines_added: f.added,
                lines_removed: f.removed,
                new_work: f.new_work,
            })
            .collect(),
    }
}
