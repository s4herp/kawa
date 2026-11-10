//! Builds throwaway git repositories with fixed authors, emails and dates, so
//! history-based tests are deterministic.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub name: String,
    pub email: String,
}

impl Author {
    pub fn new(name: &str, email: &str) -> Self {
        Self {
            name: name.to_owned(),
            email: email.to_owned(),
        }
    }
}

/// A git repository in a temporary directory, removed on drop.
pub struct FixtureRepo {
    dir: TempDir,
}

impl FixtureRepo {
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("kawa-fixture-")
            .tempdir()
            .expect("create temp dir");
        let repo = Self { dir };
        repo.git(&["init", "-q", "-b", "main"]);
        repo
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn file_path(&self, relative: &str) -> PathBuf {
        self.path().join(relative)
    }

    pub fn write(&self, relative: &str, content: &str) {
        let path = self.file_path(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, content).expect("write fixture file");
    }

    pub fn remove(&self, relative: &str) {
        self.git(&["rm", "-q", relative]);
    }

    pub fn rename(&self, from: &str, to: &str) {
        if let Some(parent) = self.file_path(to).parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        self.git(&["mv", from, to]);
    }

    /// Stages everything and commits it with the given author and ISO 8601 date
    /// used as both author and committer date. Returns the commit SHA.
    pub fn commit(&self, author: &Author, date: &str, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.commit_staged(author, date, message)
    }

    pub fn branch(&self, name: &str) {
        self.git(&["checkout", "-q", "-b", name]);
    }

    pub fn checkout(&self, name: &str) {
        self.git(&["checkout", "-q", name]);
    }

    /// Merges `branch` into the current branch with a merge commit. Files in
    /// `resolutions` are written before committing, which resolves conflicts.
    pub fn merge(
        &self,
        branch: &str,
        author: &Author,
        date: &str,
        message: &str,
        resolutions: &[(&str, &str)],
    ) -> String {
        let output = self.run(&["merge", "--no-ff", "--no-commit", "-q", branch], &[]);
        let conflicted = !output.status.success();
        if conflicted && resolutions.is_empty() {
            panic!(
                "merge of {branch} conflicted and no resolutions were given:\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
        for (path, content) in resolutions {
            self.write(path, content);
        }
        self.git(&["add", "-A"]);
        self.commit_staged(author, date, message)
    }

    /// Runs a git command in the repository and returns its trimmed stdout,
    /// panicking on failure.
    pub fn git(&self, args: &[&str]) -> String {
        let output = self.run(args, &[]);
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git output is UTF-8")
            .trim_end()
            .to_owned()
    }

    fn commit_staged(&self, author: &Author, date: &str, message: &str) -> String {
        let args = ["commit", "-q", "--allow-empty", "-m", message];
        let env = [
            ("GIT_AUTHOR_NAME", author.name.as_str()),
            ("GIT_AUTHOR_EMAIL", author.email.as_str()),
            ("GIT_AUTHOR_DATE", date),
            ("GIT_COMMITTER_NAME", author.name.as_str()),
            ("GIT_COMMITTER_EMAIL", author.email.as_str()),
            ("GIT_COMMITTER_DATE", date),
        ];
        let output = self.run(&args, &env);
        assert!(
            output.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.git(&["rev-parse", "HEAD"])
    }

    // The user's global and system git config are ignored so signing, hooks
    // or aliases on the developer machine cannot change fixture history.
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(self.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "kawa fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@kawa.invalid")
            .env("GIT_COMMITTER_NAME", "kawa fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@kawa.invalid");
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().expect("run git")
    }
}

impl Default for FixtureRepo {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_history_with_fixed_authors_dates_and_merges() {
        let ana = Author::new("Ana", "ana@example.com");
        let luis = Author::new("Luis", "luis@example.com");
        let repo = FixtureRepo::new();

        repo.write("src/a.txt", "one\ntwo\n");
        repo.commit(&ana, "2026-01-05T09:00:00", "add a");

        repo.branch("feature");
        repo.write("src/a.txt", "one\nTWO\n");
        repo.commit(&luis, "2026-01-06T10:00:00", "edit a on feature");

        repo.checkout("main");
        repo.write("src/a.txt", "ONE\ntwo\n");
        repo.commit(&ana, "2026-01-06T11:00:00", "edit a on main");

        repo.merge(
            "feature",
            &ana,
            "2026-01-07T12:00:00+00:00",
            "merge feature",
            &[("src/a.txt", "ONE\nTWO\n")],
        );
        repo.rename("src/a.txt", "src/b.txt");
        repo.commit(&luis, "2026-01-08T13:00:00", "rename a to b");

        let log = repo.git(&[
            "log",
            "--date-order",
            "--date=format:%Y-%m-%dT%H:%M:%S",
            "--format=%an|%ae|%ad|%P|%s",
        ]);
        let rows: Vec<Vec<&str>> = log.lines().map(|l| l.split('|').collect()).collect();

        let summary: Vec<(&str, &str, &str, &str)> =
            rows.iter().map(|r| (r[0], r[1], r[2], r[4])).collect();
        assert_eq!(
            summary,
            vec![
                (
                    "Luis",
                    "luis@example.com",
                    "2026-01-08T13:00:00",
                    "rename a to b"
                ),
                (
                    "Ana",
                    "ana@example.com",
                    "2026-01-07T12:00:00",
                    "merge feature"
                ),
                (
                    "Ana",
                    "ana@example.com",
                    "2026-01-06T11:00:00",
                    "edit a on main"
                ),
                (
                    "Luis",
                    "luis@example.com",
                    "2026-01-06T10:00:00",
                    "edit a on feature"
                ),
                ("Ana", "ana@example.com", "2026-01-05T09:00:00", "add a"),
            ],
            "should record every commit with the fixed author, email and date newest first"
        );
        assert_eq!(
            rows[1][3].split(' ').count(),
            2,
            "should create the merge commit with two parents"
        );
        assert_eq!(
            fs::read_to_string(repo.file_path("src/b.txt")).unwrap(),
            "ONE\nTWO\n",
            "should keep the conflict resolution content after the rename"
        );
    }
}
