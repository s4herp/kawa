//! A local copy of the head commit of each merged pull request.
//!
//! A squash merge leaves one commit on the analyzed branch; the commits the
//! author actually made stay on GitHub under `refs/pull/<n>/head`. They are
//! fetched into a bare repository next to the database that borrows the
//! analyzed repository's objects, so that repository is never written to.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use kawa_core::gix::ObjectId;

const BATCH: usize = 200;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fetched {
    pub wanted: u64,
    pub downloaded: u64,
    pub missing: u64,
}

pub fn cache_path(root: &Path, github: &str) -> PathBuf {
    root.join(format!("{}.git", github.replace('/', "__")))
}

fn git(directory: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// Heads already in the cache, by pull request number.
pub fn read(cache: &Path) -> HashMap<u64, ObjectId> {
    let Ok(listing) = git(
        cache,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/pull/",
        ],
    ) else {
        return HashMap::new();
    };
    listing
        .lines()
        .filter_map(|line| {
            let (name, id) = line.split_once(' ')?;
            let number = name.strip_prefix("refs/pull/")?.strip_suffix("/head")?;
            Some((
                number.parse().ok()?,
                ObjectId::from_hex(id.as_bytes()).ok()?,
            ))
        })
        .collect()
}

fn prepare(cache: &Path, repository: &Path) -> Result<(), String> {
    if !cache.join("objects").is_dir() {
        std::fs::create_dir_all(cache)
            .map_err(|e| format!("cannot create {}: {e}", cache.display()))?;
        git(cache, &["init", "--bare", "--quiet"])?;
    }
    let common = PathBuf::from(git(repository, &["rev-parse", "--git-common-dir"])?);
    let common = if common.is_absolute() {
        common
    } else {
        repository.join(common)
    };
    let objects = common
        .join("objects")
        .canonicalize()
        .map_err(|e| format!("cannot locate the objects of {}: {e}", repository.display()))?;
    let info = cache.join("objects").join("info");
    std::fs::create_dir_all(&info).map_err(|e| e.to_string())?;
    std::fs::write(info.join("alternates"), format!("{}\n", objects.display()))
        .map_err(|e| e.to_string())
}

fn refspec(number: u64) -> String {
    format!("+refs/pull/{number}/head:refs/pull/{number}/head")
}

fn download(cache: &Path, remote: &str, numbers: &[u64]) -> Result<(), String> {
    let refspecs: Vec<String> = numbers.iter().copied().map(refspec).collect();
    let mut args = vec!["fetch", "--quiet", "--no-tags", remote];
    args.extend(refspecs.iter().map(String::as_str));
    git(cache, &args).map(|_| ())
}

/// Downloads the heads of `numbers` that the cache does not have yet from
/// `remote`. A head GitHub no longer serves is counted as missing.
pub fn fetch(
    cache: &Path,
    repository: &Path,
    remote: &str,
    numbers: &[u64],
) -> Result<Fetched, String> {
    prepare(cache, repository)?;
    let known = read(cache);
    let wanted: Vec<u64> = numbers
        .iter()
        .copied()
        .filter(|n| !known.contains_key(n))
        .collect();
    for batch in wanted.chunks(BATCH) {
        if download(cache, remote, batch).is_err() {
            // One unknown ref fails the whole batch.
            for number in batch {
                let _ = download(cache, remote, &[*number]);
            }
        }
    }
    let after = read(cache);
    let missing = numbers.iter().filter(|n| !after.contains_key(n)).count() as u64;
    Ok(Fetched {
        wanted: wanted.len() as u64,
        downloaded: wanted.len() as u64 - missing.min(wanted.len() as u64),
        missing,
    })
}

/// The URL the analyzed repository fetches from.
pub fn remote_url(repository: &Path, github: &str) -> String {
    git(repository, &["remote", "get-url", "origin"])
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| format!("https://github.com/{github}.git"))
}
