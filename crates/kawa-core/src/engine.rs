//! Incremental line-origin replay over git history.
//!
//! For every file touched since the boundary the engine keeps the origin of
//! each line. Replaying a commit applies its diff to those origins: removed
//! lines produce removal records, added lines get the commit as origin. Files
//! never touched since the boundary have no entry; their lines get
//! deterministic baseline ids derived from the blob, so every branch that
//! later touches the same file sees the same ids.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, HashMap, HashSet};
use std::sync::Arc;

use gix::ObjectId;
use gix::bstr::{BStr, BString, ByteSlice};
use gix::diff::blob::{Algorithm, Diff, InternedInput, Interner};
use gix::object::tree::diff::ChangeDetached;
use globset::GlobSet;
use thiserror::Error;

type LineId = u64;
type State = imbl::HashMap<BString, Arc<[LineId]>>;

const BASELINE_BIT: LineId = 1 << 63;
const BINARY_PROBE_BYTES: usize = 8000;
const RENAME_SIMILARITY: f32 = 0.5;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("git error: {0}")]
    Git(String),
}

fn git_err(error: impl std::fmt::Display) -> EngineError {
    EngineError::Git(error.to_string())
}

pub struct ReplayOptions {
    /// Commits with a committer time before this (seconds since epoch) are
    /// not replayed; their trees are the baseline.
    pub since: i64,
    pub excluded: GlobSet,
    pub ignore_revs: HashSet<ObjectId>,
    /// Head commit of each merged pull request, by number. A squash commit
    /// whose subject ends in `(#N)` is replayed as a merge of its parent and
    /// that head, so the commits of the pull request keep their authorship,
    /// dates and the rework between them.
    pub pr_heads: HashMap<u64, ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Introducer {
    /// The line existed before the replay boundary.
    Baseline,
    Commit(ObjectId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub old_path: Option<String>,
    pub added: u32,
    pub removed: u32,
    /// Added lines that do not replace a removed line in the same hunk.
    pub new_work: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRecord {
    pub id: ObjectId,
    pub parents: Vec<ObjectId>,
    pub author_name: String,
    pub author_email: String,
    pub author_time: i64,
    pub author_offset: i32,
    pub subject: String,
    pub is_merge: bool,
    pub is_ignored: bool,
    pub files: Vec<FileChange>,
}

/// Lines introduced by `introduced` and first removed by `removed`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RemovalPair {
    pub introduced: Introducer,
    pub removed: ObjectId,
    pub lines: u64,
}

#[derive(Debug, Default)]
pub struct Replay {
    /// In replay (topological) order.
    pub commits: Vec<CommitRecord>,
    /// Sorted, for deterministic output.
    pub removals: Vec<RemovalPair>,
}

/// Built-in exclusions: lockfiles, vendored and generated code.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    "**/*.lock",
    "**/*.lock.hcl",
    "**/package-lock.json",
    "**/npm-shrinkwrap.json",
    "**/pnpm-lock.yaml",
    "**/go.sum",
    "**/vendor/**",
    "**/node_modules/**",
    "**/third_party/**",
    "**/generated/**",
    "**/*.min.js",
    "**/*.min.css",
];

pub fn path_filter(extra: &[String]) -> Result<GlobSet, globset::Error> {
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in DEFAULT_EXCLUDES
        .iter()
        .copied()
        .chain(extra.iter().map(String::as_str))
    {
        builder.add(globset::Glob::new(pattern)?);
    }
    builder.build()
}

/// Replays the history reachable from `tip` back to `options.since`.
pub fn replay(
    repo: &gix::Repository,
    tip: ObjectId,
    options: &ReplayOptions,
) -> Result<Replay, EngineError> {
    let order = topological_range(repo, tip, options)?;
    let mut engine = Engine {
        repo,
        options,
        blob_index: HashMap::new(),
        introducers: Vec::new(),
        removals: HashMap::new(),
        commits: Vec::with_capacity(order.len()),
    };
    engine.run(&order)?;
    Ok(engine.finish())
}

struct RangeCommit {
    id: ObjectId,
    parents: Vec<ObjectId>,
}

struct Graph {
    parents: HashMap<ObjectId, Vec<ObjectId>>,
    times: HashMap<ObjectId, i64>,
}

/// The pull request number GitHub appends to a squash commit subject.
fn squashed_pr(subject: &BStr) -> Option<u64> {
    let subject = subject.to_str_lossy();
    let number = subject.trim_end().strip_suffix(')')?.rsplit_once("(#")?.1;
    number.parse().ok()
}

/// Parents of every commit reachable from `tip` with committer time at or
/// after `since`, with their commit times. With `pr_heads`, a squash commit
/// also gets the head of its pull request as a parent.
fn reachable(
    repo: &gix::Repository,
    tip: ObjectId,
    since: i64,
    pr_heads: Option<&HashMap<u64, ObjectId>>,
) -> Result<Graph, EngineError> {
    let mut parents: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    let mut times: HashMap<ObjectId, i64> = HashMap::new();
    let mut stack = vec![tip];
    while let Some(id) = stack.pop() {
        if parents.contains_key(&id) {
            continue;
        }
        let commit = repo.find_commit(id).map_err(git_err)?;
        let time = commit.time().map_err(git_err)?.seconds;
        if time < since && id != tip {
            continue;
        }
        let mut ids: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
        if let (Some(heads), [parent]) = (pr_heads, ids.as_slice()) {
            let head = squashed_pr(commit.message().map_err(git_err)?.summary().as_ref())
                .and_then(|number| heads.get(&number))
                .filter(|head| **head != id && *head != parent);
            // A head committed after the squash was pushed after the merge and
            // may descend from it; a head that was never fetched is skipped.
            if let Some(head) = head
                && let Ok(head_commit) = repo.find_commit(*head)
                && head_commit.time().map_err(git_err)?.seconds <= time
            {
                ids.push(*head);
            }
        }
        stack.extend(ids.iter().copied());
        times.insert(id, time);
        parents.insert(id, ids);
    }
    Ok(Graph { parents, times })
}

/// Commits to replay, in an order where every parent comes before its
/// children; ties go to the older commit, then the smaller id.
fn topological_range(
    repo: &gix::Repository,
    tip: ObjectId,
    options: &ReplayOptions,
) -> Result<Vec<RangeCommit>, EngineError> {
    let graph = reachable(repo, tip, options.since, Some(&options.pr_heads))?;
    let order = sorted(&graph);
    if order.len() == graph.parents.len() {
        return Ok(order);
    }
    // A pull request head that descends from its own squash commit makes a
    // cycle; real history has none, so fall back to it.
    Ok(sorted(&reachable(repo, tip, options.since, None)?))
}

fn sorted(graph: &Graph) -> Vec<RangeCommit> {
    let Graph { parents, times } = graph;
    let mut pending: HashMap<ObjectId, usize> = HashMap::new();
    let mut children: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    for (id, ids) in parents {
        let in_range: BTreeSet<&ObjectId> =
            ids.iter().filter(|p| parents.contains_key(*p)).collect();
        pending.insert(*id, in_range.len());
        for parent in in_range {
            children.entry(*parent).or_default().push(*id);
        }
    }
    let mut ready: BinaryHeap<Reverse<(i64, ObjectId)>> = pending
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(id, _)| Reverse((times[id], *id)))
        .collect();
    let mut order = Vec::with_capacity(parents.len());
    while let Some(Reverse((_, id))) = ready.pop() {
        for child in children.get(&id).into_iter().flatten() {
            let count = pending.get_mut(child).expect("child is in range");
            *count -= 1;
            if *count == 0 {
                ready.push(Reverse((times[child], *child)));
            }
        }
        order.push(RangeCommit {
            id,
            parents: parents[&id].clone(),
        });
    }
    order
}

struct Engine<'r> {
    repo: &'r gix::Repository,
    options: &'r ReplayOptions,
    /// Baseline files seen so far; identical blobs at different paths are
    /// different files and must not share line ids.
    blob_index: HashMap<(BString, ObjectId), u32>,
    /// Commit index (into `commits`) that introduced each non-baseline line id.
    introducers: Vec<u32>,
    /// Earliest removing commit index per line id.
    removals: HashMap<LineId, u32>,
    commits: Vec<CommitRecord>,
}

/// One version of a file: normalized line keys plus the origin of each line.
struct Version {
    keys: Vec<Vec<u8>>,
    origins: Arc<[LineId]>,
}

impl Engine<'_> {
    fn run(&mut self, order: &[RangeCommit]) -> Result<(), EngineError> {
        let ids: HashSet<ObjectId> = order.iter().map(|c| c.id).collect();
        let mut remaining_children: HashMap<ObjectId, usize> = HashMap::new();
        for commit in order {
            for parent in commit.parents.iter().filter(|p| ids.contains(*p)) {
                *remaining_children.entry(*parent).or_default() += 1;
            }
        }
        let mut states: HashMap<ObjectId, State> = HashMap::new();
        for commit in order {
            let parent_states: Vec<State> = commit
                .parents
                .iter()
                .map(|p| states.get(p).cloned().unwrap_or_default())
                .collect();
            let state = self.replay_commit(commit, parent_states)?;
            for parent in commit.parents.iter().filter(|p| ids.contains(*p)) {
                let left = remaining_children.get_mut(parent).expect("counted");
                *left -= 1;
                if *left == 0 {
                    states.remove(parent);
                }
            }
            states.insert(commit.id, state);
        }
        Ok(())
    }

    fn finish(self) -> Replay {
        let mut pairs: HashMap<(Introducer, ObjectId), u64> = HashMap::new();
        for (line, remover) in &self.removals {
            let introduced = if line & BASELINE_BIT != 0 {
                Introducer::Baseline
            } else {
                let introducer = &self.commits[self.introducers[*line as usize] as usize];
                // An ignored revision emits no events, so it introduces nothing.
                if introducer.is_ignored {
                    Introducer::Baseline
                } else {
                    Introducer::Commit(introducer.id)
                }
            };
            *pairs
                .entry((introduced, self.commits[*remover as usize].id))
                .or_default() += 1;
        }
        let mut removals: Vec<RemovalPair> = pairs
            .into_iter()
            .map(|((introduced, removed), lines)| RemovalPair {
                introduced,
                removed,
                lines,
            })
            .collect();
        removals.sort();
        Replay {
            commits: self.commits,
            removals,
        }
    }

    fn replay_commit(
        &mut self,
        range_commit: &RangeCommit,
        mut parent_states: Vec<State>,
    ) -> Result<State, EngineError> {
        let commit = self.repo.find_commit(range_commit.id).map_err(git_err)?;
        let author = commit.author().map_err(git_err)?;
        let author_time = author.time().map_err(git_err)?;
        let index = u32::try_from(self.commits.len()).expect("fewer than 4 billion commits");
        let is_ignored = self.options.ignore_revs.contains(&range_commit.id);
        self.commits.push(CommitRecord {
            id: range_commit.id,
            parents: range_commit.parents.clone(),
            author_name: author.name.trim().to_str_lossy().into_owned(),
            author_email: author.email.trim().to_str_lossy().into_owned(),
            author_time: author_time.seconds,
            author_offset: author_time.offset,
            subject: commit
                .message()
                .map_err(git_err)?
                .summary()
                .to_str_lossy()
                .into_owned(),
            is_merge: range_commit.parents.len() > 1,
            is_ignored,
            files: Vec::new(),
        });
        let tree = commit.tree().map_err(git_err)?;

        if range_commit.parents.len() > 1 {
            let parent_trees = range_commit
                .parents
                .iter()
                .map(|p| {
                    self.repo
                        .find_commit(*p)
                        .map_err(git_err)?
                        .tree()
                        .map_err(git_err)
                })
                .collect::<Result<Vec<_>, _>>()?;
            return self.replay_merge(index, &tree, &parent_trees, parent_states);
        }

        let mut state = parent_states.pop().unwrap_or_default();
        let parent_tree = match range_commit.parents.first() {
            Some(parent) => Some(
                self.repo
                    .find_commit(*parent)
                    .map_err(git_err)?
                    .tree()
                    .map_err(git_err)?,
            ),
            None => None,
        };
        let mut options = gix::diff::Options::default();
        options.track_rewrites(Some(gix::diff::Rewrites {
            copies: None,
            percentage: Some(RENAME_SIMILARITY),
            limit: 1000,
            track_empty: false,
        }));
        let changes = self
            .repo
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(options))
            .map_err(git_err)?;
        let mut files = Vec::new();
        for change in changes {
            if let Some(file) = self.apply_change(index, is_ignored, &mut state, change)? {
                files.push(file);
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        if !is_ignored {
            self.commits[index as usize].files = files;
        }
        Ok(state)
    }

    fn apply_change(
        &mut self,
        index: u32,
        ignored: bool,
        state: &mut State,
        change: ChangeDetached,
    ) -> Result<Option<FileChange>, EngineError> {
        let (old, new) = match change {
            ChangeDetached::Addition {
                location,
                entry_mode,
                id,
                ..
            } if entry_mode.is_blob() => (None, Some((location, id))),
            ChangeDetached::Deletion {
                location,
                entry_mode,
                id,
                ..
            } if entry_mode.is_blob() => (Some((location, id)), None),
            ChangeDetached::Modification {
                location,
                previous_entry_mode,
                previous_id,
                entry_mode,
                id,
            } => (
                previous_entry_mode
                    .is_blob()
                    .then(|| (location.clone(), previous_id)),
                entry_mode.is_blob().then_some((location, id)),
            ),
            ChangeDetached::Rewrite {
                source_location,
                source_entry_mode,
                source_id,
                entry_mode,
                id,
                location,
                copy,
                ..
            } => (
                (!copy && source_entry_mode.is_blob()).then_some((source_location, source_id)),
                entry_mode.is_blob().then_some((location, id)),
            ),
            _ => (None, None),
        };
        let old = old.filter(|(path, _)| !self.is_excluded(path.as_ref()));
        let new = new.filter(|(path, _)| !self.is_excluded(path.as_ref()));

        let old_version = match &old {
            Some((path, blob)) => {
                let version = self.version(state, path.as_ref(), *blob)?;
                state.remove(path);
                version
            }
            None => None,
        };
        let new_keys = match &new {
            Some((_, blob)) => self.keys(*blob)?,
            None => None,
        };

        let Some((new_path, _)) = new else {
            // Deleted, or turned into something that is not text.
            if let Some(version) = old_version {
                let removed = self.remove_all(index, ignored, &version);
                let (path, _) = old.expect("old version implies old path");
                return Ok(Some(FileChange {
                    path: path.to_string(),
                    old_path: None,
                    added: 0,
                    removed,
                    new_work: 0,
                }));
            }
            return Ok(None);
        };
        let Some(new_keys) = new_keys else {
            if let Some(version) = old_version {
                self.remove_all(index, ignored, &version);
            }
            return Ok(None);
        };

        let old_version = old_version.unwrap_or(Version {
            keys: Vec::new(),
            origins: Arc::from([]),
        });
        let (origins, added, removed, new_work) =
            self.apply_diff(index, ignored, &old_version, &new_keys);
        state.insert(new_path.clone(), origins);
        let old_path = old
            .map(|(path, _)| path)
            .filter(|path| *path != new_path)
            .map(|path| path.to_string());
        if added == 0 && removed == 0 {
            return Ok(None);
        }
        Ok(Some(FileChange {
            path: new_path.to_string(),
            old_path,
            added,
            removed,
            new_work,
        }))
    }

    fn replay_merge(
        &mut self,
        index: u32,
        tree: &gix::Tree<'_>,
        parent_trees: &[gix::Tree<'_>],
        parent_states: Vec<State>,
    ) -> Result<State, EngineError> {
        let mut paths: BTreeSet<BString> = BTreeSet::new();
        for parent_tree in parent_trees {
            let mut options = gix::diff::Options::default();
            options.track_rewrites(None);
            for change in self
                .repo
                .diff_tree_to_tree(Some(parent_tree), Some(tree), Some(options))
                .map_err(git_err)?
            {
                let touches_blob = match &change {
                    ChangeDetached::Modification {
                        previous_entry_mode,
                        entry_mode,
                        ..
                    } => previous_entry_mode.is_blob() || entry_mode.is_blob(),
                    other => other.entry_mode().is_blob(),
                };
                if touches_blob {
                    paths.insert(change.location().to_owned());
                }
            }
        }
        let mut state = parent_states.first().cloned().unwrap_or_default();
        let mut files = Vec::new();
        for path in paths {
            if self.is_excluded(path.as_ref()) {
                continue;
            }
            let result_blob = blob_at(tree, path.as_ref())?;
            let mut parent_blobs = Vec::with_capacity(parent_trees.len());
            for parent_tree in parent_trees {
                parent_blobs.push(blob_at(parent_tree, path.as_ref())?);
            }
            // A path the merge left exactly as one parent had it is that
            // parent's history, not a change by the merge: adopt its origins.
            if let Some(same) = parent_blobs.iter().position(|blob| *blob == result_blob) {
                match parent_states[same].get(&path) {
                    Some(origins) if result_blob.is_some() => {
                        state.insert(path.clone(), origins.clone());
                    }
                    _ => {
                        state.remove(&path);
                    }
                }
                continue;
            }
            let mut parents = Vec::new();
            for (blob, parent_state) in parent_blobs.iter().zip(&parent_states) {
                if let Some(blob) = blob
                    && let Some(version) = self.version(parent_state, path.as_ref(), *blob)?
                {
                    parents.push(version);
                }
            }
            let new_keys = match result_blob {
                Some(blob) => self.keys(blob)?,
                None => None,
            };
            let previous: HashSet<LineId> = parents
                .iter()
                .flat_map(|v| v.origins.iter().copied())
                .collect();
            let Some(new_keys) = new_keys else {
                state.remove(&path);
                let mut removed = 0;
                for line in previous {
                    if self.removed_by_merge(line, index) {
                        removed += 1;
                    }
                }
                if removed > 0 {
                    files.push(FileChange {
                        path: path.to_string(),
                        old_path: None,
                        added: 0,
                        removed,
                        new_work: 0,
                    });
                }
                continue;
            };

            let mut origins: Vec<Option<LineId>> = vec![None; new_keys.len()];
            // Ids of every parent line that survives, including the ones a
            // result line did not take because another parent matched first.
            let mut surviving: HashSet<LineId> = HashSet::new();
            for parent in &parents {
                for (old_line, new_line) in unchanged_pairs(&parent.keys, &new_keys) {
                    origins[new_line].get_or_insert(parent.origins[old_line]);
                    surviving.insert(parent.origins[old_line]);
                }
            }
            let mut added = 0u32;
            let origins: Arc<[LineId]> = origins
                .into_iter()
                .map(|origin| {
                    origin.unwrap_or_else(|| {
                        added += 1;
                        self.new_line(index)
                    })
                })
                .collect();
            let mut removed = 0u32;
            for line in previous.difference(&surviving) {
                if self.removed_by_merge(*line, index) {
                    removed += 1;
                }
            }
            state.insert(path.clone(), origins);
            if added > 0 || removed > 0 {
                files.push(FileChange {
                    path: path.to_string(),
                    old_path: None,
                    added,
                    removed,
                    new_work: added.saturating_sub(removed),
                });
            }
        }
        self.commits[index as usize].files = files;
        Ok(state)
    }

    /// Applies a diff from `old` to `new_keys`. Returns the new origins and
    /// the added, removed and new-work line counts.
    fn apply_diff(
        &mut self,
        index: u32,
        ignored: bool,
        old: &Version,
        new_keys: &[Vec<u8>],
    ) -> (Arc<[LineId]>, u32, u32, u32) {
        let mut origins = Vec::with_capacity(new_keys.len());
        let (mut added, mut removed, mut new_work) = (0u32, 0u32, 0u32);
        let mut cursor = 0usize;
        for hunk in hunks(&old.keys, new_keys) {
            let (before, after) = (
                hunk.before.start as usize..hunk.before.end as usize,
                hunk.after.start as usize..hunk.after.end as usize,
            );
            origins.extend_from_slice(&old.origins[cursor..before.start]);
            cursor = before.end;
            let old_lines = &old.origins[before.clone()];
            if ignored {
                let fallback = old_lines
                    .last()
                    .copied()
                    .or_else(|| origins.last().copied())
                    .or_else(|| old.origins.get(before.end).copied());
                for offset in 0..after.len() {
                    let inherited = old_lines.get(offset).copied().or(fallback);
                    origins.push(inherited.unwrap_or_else(|| self.new_line(index)));
                }
                continue;
            }
            for line in old_lines {
                self.record_removal(*line, index);
            }
            for _ in after.clone() {
                origins.push(self.new_line(index));
            }
            let (hunk_added, hunk_removed) = (after.len() as u32, before.len() as u32);
            added += hunk_added;
            removed += hunk_removed;
            new_work += hunk_added.saturating_sub(hunk_removed);
        }
        origins.extend_from_slice(&old.origins[cursor..]);
        (origins.into(), added, removed, new_work)
    }

    fn remove_all(&mut self, index: u32, ignored: bool, version: &Version) -> u32 {
        if ignored {
            return 0;
        }
        for line in version.origins.iter() {
            self.record_removal(*line, index);
        }
        version.origins.len() as u32
    }

    /// A line missing from a merge result that a branch already removed was
    /// removed by that branch, not by the merge.
    fn removed_by_merge(&mut self, line: LineId, index: u32) -> bool {
        if self.removals.contains_key(&line) {
            return false;
        }
        self.removals.insert(line, index);
        true
    }

    fn new_line(&mut self, index: u32) -> LineId {
        let id = self.introducers.len() as LineId;
        self.introducers.push(index);
        id
    }

    /// Keeps only the earliest removal of each line, by author time, so a
    /// line removed on two branches counts once.
    fn record_removal(&mut self, line: LineId, index: u32) {
        let time = self.commits[index as usize].author_time;
        self.removals
            .entry(line)
            .and_modify(|current| {
                if time < self.commits[*current as usize].author_time {
                    *current = index;
                }
            })
            .or_insert(index);
    }

    /// The version of `path` at `blob` with its origins: from `state` when the
    /// file was touched since the boundary, baseline ids otherwise. `None` for
    /// binary files.
    fn version(
        &mut self,
        state: &State,
        path: &BStr,
        blob: ObjectId,
    ) -> Result<Option<Version>, EngineError> {
        let Some(keys) = self.keys(blob)? else {
            return Ok(None);
        };
        let origins = match state.get(path) {
            Some(origins) if origins.len() == keys.len() => origins.clone(),
            _ => {
                let next = self.blob_index.len() as u32;
                let blob_number = *self
                    .blob_index
                    .entry((path.to_owned(), blob))
                    .or_insert(next);
                (0..keys.len() as u64)
                    .map(|line| BASELINE_BIT | (u64::from(blob_number) << 32) | line)
                    .collect()
            }
        };
        Ok(Some(Version { keys, origins }))
    }

    /// Whitespace-free line keys of a blob, or `None` if it is binary.
    fn keys(&self, blob: ObjectId) -> Result<Option<Vec<Vec<u8>>>, EngineError> {
        let object = self.repo.find_object(blob).map_err(git_err)?;
        let data = &object.data;
        if data[..data.len().min(BINARY_PROBE_BYTES)].contains(&0) {
            return Ok(None);
        }
        Ok(Some(
            data.lines()
                .map(|line| {
                    line.iter()
                        .copied()
                        .filter(|b| !b.is_ascii_whitespace())
                        .collect()
                })
                .collect(),
        ))
    }

    fn is_excluded(&self, path: &BStr) -> bool {
        self.options.excluded.is_match(path.to_str_lossy().as_ref())
    }
}

fn blob_at(tree: &gix::Tree<'_>, path: &BStr) -> Result<Option<ObjectId>, EngineError> {
    let entry = tree
        .lookup_entry_by_path(path.to_str_lossy().as_ref())
        .map_err(git_err)?;
    Ok(entry.filter(|e| e.mode().is_blob()).map(|e| e.object_id()))
}

fn hunks(old: &[Vec<u8>], new: &[Vec<u8>]) -> Vec<gix::diff::blob::Hunk> {
    let mut input = InternedInput {
        before: Vec::new(),
        after: Vec::new(),
        interner: Interner::new(old.len() + new.len()),
    };
    input.update_before(old.iter().map(Vec::as_slice));
    input.update_after(new.iter().map(Vec::as_slice));
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    diff.hunks().collect()
}

/// Index pairs `(old, new)` of lines left unchanged by the diff.
fn unchanged_pairs(old: &[Vec<u8>], new: &[Vec<u8>]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    for hunk in hunks(old, new) {
        while i < hunk.before.start as usize {
            pairs.push((i, j));
            i += 1;
            j += 1;
        }
        i = hunk.before.end as usize;
        j = hunk.after.end as usize;
    }
    while i < old.len() && j < new.len() {
        pairs.push((i, j));
        i += 1;
        j += 1;
    }
    pairs
}
