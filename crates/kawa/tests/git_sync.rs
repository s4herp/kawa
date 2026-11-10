//! `repository-ingestion` git scenarios.

use std::fs;

use jiff::Timestamp;
use kawa::config::{Config, Repository};
use kawa::sync::{self, RepositoryOutcome};
use kawa_store::Store;
use kawa_testkit::{Author, FixtureRepo};

fn now() -> Timestamp {
    "2026-02-01T00:00:00Z".parse().unwrap()
}

fn config_for(repositories: Vec<Repository>) -> Config {
    Config {
        repositories,
        ..Config::default()
    }
}

fn repository(path: &std::path::Path, branch: &str) -> Repository {
    Repository {
        path: path.to_owned(),
        branch: Some(branch.to_owned()),
        github: None,
    }
}

#[test]
fn reads_the_configured_branch_and_leaves_the_clone_untouched() {
    let fixture = FixtureRepo::new();
    let ana = Author::new("Ana", "ana@example.com");
    fixture.write("src/a.rs", "a\nb\n");
    fixture.commit(&ana, "2026-01-10T10:00:00+00:00", "add");
    fixture.write("src/a.rs", "a\nB\n");
    let main_head = fixture.commit(&ana, "2026-01-11T10:00:00+00:00", "edit");
    fixture.git(&["checkout", "-q", "-b", "wip", "HEAD~1"]);
    fixture.write("src/a.rs", "x\n");
    fixture.commit(&ana, "2026-01-12T10:00:00+00:00", "wip");
    fixture.write("src/a.rs", "uncommitted\n");
    fixture.write("src/staged.rs", "staged\n");
    fixture.git(&["add", "src/staged.rs"]);

    let snapshot = || {
        (
            fixture.git(&["show-ref"]),
            fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]),
            fixture.git(&["status", "--porcelain"]),
            fs::read(fixture.path().join(".git/index")).unwrap(),
            fs::read_to_string(fixture.file_path("src/a.rs")).unwrap(),
        )
    };
    let before = snapshot();

    let mut store = Store::open_in_memory().unwrap();
    let outcomes = sync::sync_git(
        &mut store,
        &config_for(vec![repository(fixture.path(), "main")]),
        now(),
    )
    .unwrap();

    assert_eq!(
        outcomes,
        vec![RepositoryOutcome::Synced {
            path: fixture.path().display().to_string(),
            head: main_head.clone(),
            commits: 2,
        }],
        "should sync the two commits of main, not the checked-out wip branch"
    );
    assert_eq!(
        snapshot(),
        before,
        "should leave branches, index and working tree unchanged"
    );

    let status = store.sync_status(1).unwrap();
    assert_eq!(
        status.git_head.as_deref(),
        Some(main_head.as_str()),
        "should record the synced head"
    );
    assert_eq!(
        status.git_synced_at,
        Some(now()),
        "should record the sync time"
    );
    assert_eq!(
        store.removal_pairs(1).unwrap().len(),
        1,
        "should store the removal of the edited line"
    );
}

#[test]
fn a_path_that_is_not_a_repository_fails_alone() {
    let not_a_repo = tempfile::tempdir().unwrap();
    let fixture = FixtureRepo::new();
    fixture.write("a.txt", "a\n");
    fixture.commit(
        &Author::new("Ana", "ana@example.com"),
        "2026-01-10T10:00:00+00:00",
        "add",
    );

    let mut store = Store::open_in_memory().unwrap();
    let outcomes = sync::sync_git(
        &mut store,
        &config_for(vec![
            repository(not_a_repo.path(), "main"),
            repository(fixture.path(), "main"),
        ]),
        now(),
    )
    .unwrap();

    match &outcomes[0] {
        RepositoryOutcome::Failed { path, error } => {
            assert_eq!(
                path,
                &not_a_repo.path().display().to_string(),
                "should name the failed path"
            );
            assert!(
                error.contains("not a git repository"),
                "should explain the failure, got {error}"
            );
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    assert!(
        matches!(outcomes[1], RepositoryOutcome::Synced { commits: 1, .. }),
        "should continue with the other repositories"
    );
    let status = store.sync_status(1).unwrap();
    assert!(
        status
            .error
            .as_ref()
            .is_some_and(|e| e.message.contains("not a git repository")),
        "should record the failure in sync status"
    );
}

#[test]
fn resync_replaces_history_instead_of_duplicating() {
    let fixture = FixtureRepo::new();
    let ana = Author::new("Ana", "ana@example.com");
    fixture.write("a.txt", "a\n");
    fixture.commit(&ana, "2026-01-10T10:00:00+00:00", "add");
    let config = config_for(vec![repository(fixture.path(), "main")]);
    let mut store = Store::open_in_memory().unwrap();

    sync::sync_git(&mut store, &config, now()).unwrap();
    fixture.write("a.txt", "b\n");
    fixture.commit(&ana, "2026-01-11T10:00:00+00:00", "edit");
    sync::sync_git(&mut store, &config, now()).unwrap();

    assert_eq!(
        store.commit_count(1).unwrap(),
        2,
        "should hold each commit once after two syncs"
    );
    assert_eq!(
        store
            .removal_pairs(1)
            .unwrap()
            .iter()
            .map(|p| p.2)
            .sum::<u64>(),
        1,
        "should not double count removals"
    );
}

#[test]
fn boundary_includes_window_and_recency() {
    let config = Config::default();
    let expected: Timestamp = "2025-07-15T00:00:00Z".parse().unwrap();
    assert_eq!(
        sync::boundary(&config, now()),
        expected.as_second(),
        "should start replay 180 + 21 days before now"
    );
}
