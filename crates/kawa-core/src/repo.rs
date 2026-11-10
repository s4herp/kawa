//! Read-only access to repository content at a branch tip, never the working tree.

use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RepoError {
    #[error("{path} is not a git repository: {message}")]
    Open { path: String, message: String },
    #[error("branch {branch:?} not found: {message}")]
    Branch { branch: String, message: String },
    #[error("git object error: {0}")]
    Object(String),
}

pub fn open(path: &Path) -> Result<gix::Repository, RepoError> {
    gix::open(path).map_err(|error| RepoError::Open {
        path: path.display().to_string(),
        message: error.to_string(),
    })
}

/// Resolves `branch` (a short name, `refs/...` path or `None` for the remote
/// default branch, falling back to `HEAD`) to its tip commit.
pub fn branch_tip<'r>(
    repo: &'r gix::Repository,
    branch: Option<&str>,
) -> Result<gix::Commit<'r>, RepoError> {
    let candidates: Vec<String> = match branch {
        Some(name) if name.starts_with("refs/") => vec![name.to_owned()],
        Some(name) => vec![
            format!("refs/heads/{name}"),
            format!("refs/remotes/origin/{name}"),
        ],
        None => vec!["refs/remotes/origin/HEAD".to_owned(), "HEAD".to_owned()],
    };
    let wanted = branch.unwrap_or("default branch").to_owned();
    for candidate in &candidates {
        if let Ok(mut reference) = repo.find_reference(candidate.as_str()) {
            return reference
                .peel_to_commit()
                .map_err(|error| RepoError::Object(error.to_string()));
        }
    }
    if branch.is_none() {
        return repo.head_commit().map_err(|error| RepoError::Branch {
            branch: wanted,
            message: error.to_string(),
        });
    }
    Err(RepoError::Branch {
        branch: wanted,
        message: format!("tried {}", candidates.join(", ")),
    })
}

/// Returns the `.mailmap` content committed at `commit`, if any.
pub fn mailmap_at(commit: &gix::Commit<'_>) -> Result<Option<Vec<u8>>, RepoError> {
    file_at(commit, ".mailmap")
}

/// Commit ids listed in `.git-blame-ignore-revs` at `commit`. Comments,
/// blank lines and entries that are not full hex ids are skipped.
pub fn ignore_revs_at(
    commit: &gix::Commit<'_>,
) -> Result<std::collections::HashSet<gix::ObjectId>, RepoError> {
    let Some(data) = file_at(commit, ".git-blame-ignore-revs")? else {
        return Ok(Default::default());
    };
    Ok(String::from_utf8_lossy(&data)
        .lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter_map(|hex| gix::ObjectId::from_hex(hex.as_bytes()).ok())
        .collect())
}

fn file_at(commit: &gix::Commit<'_>, path: &str) -> Result<Option<Vec<u8>>, RepoError> {
    let tree = commit
        .tree()
        .map_err(|error| RepoError::Object(error.to_string()))?;
    let Some(entry) = tree
        .lookup_entry_by_path(path)
        .map_err(|error| RepoError::Object(error.to_string()))?
    else {
        return Ok(None);
    };
    let object = entry
        .object()
        .map_err(|error| RepoError::Object(error.to_string()))?;
    Ok(Some(object.data.clone()))
}
