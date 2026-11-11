//! Spec scenarios of `line-survival` on fixture repositories.

use std::collections::HashSet;
use std::fs;

use gix::ObjectId;
use kawa_core::engine::{self, FileChange, Introducer, RemovalPair, Replay, ReplayOptions};
use kawa_core::repo;
use kawa_testkit::{Author, FixtureRepo};

fn ana() -> Author {
    Author::new("Ana", "ana@example.com")
}

fn luis() -> Author {
    Author::new("Luis", "luis@example.com")
}

fn day(n: u32) -> String {
    format!("2026-01-{:02}T10:00:00+00:00", n)
}

fn oid(hex: &str) -> ObjectId {
    ObjectId::from_hex(hex.as_bytes()).unwrap()
}

fn options(since: i64, extra_ignored: &[String]) -> ReplayOptions {
    ReplayOptions {
        since,
        excluded: engine::path_filter(&[]).unwrap(),
        ignore_revs: extra_ignored.iter().map(|h| oid(h)).collect(),
    }
}

fn replay_with(fixture: &FixtureRepo, options: &ReplayOptions) -> Replay {
    let repository = repo::open(fixture.path()).unwrap();
    let tip = repo::branch_tip(&repository, Some("main")).unwrap();
    let mut options_with_file = ReplayOptions {
        since: options.since,
        excluded: options.excluded.clone(),
        ignore_revs: options.ignore_revs.clone(),
    };
    options_with_file
        .ignore_revs
        .extend(repo::ignore_revs_at(&tip).unwrap());
    engine::replay(&repository, tip.id, &options_with_file).unwrap()
}

fn replay_all(fixture: &FixtureRepo) -> Replay {
    replay_with(fixture, &options(0, &[]))
}

fn files_of<'a>(replay: &'a Replay, sha: &str) -> &'a [FileChange] {
    &replay
        .commits
        .iter()
        .find(|c| c.id == oid(sha))
        .unwrap_or_else(|| panic!("commit {sha} not replayed"))
        .files
}

fn pair(introduced: Option<&str>, removed: &str, lines: u64) -> RemovalPair {
    RemovalPair {
        introduced: introduced.map_or(Introducer::Baseline, |h| Introducer::Commit(oid(h))),
        removed: oid(removed),
        lines,
    }
}

fn lines(range: std::ops::Range<u32>) -> String {
    range.map(|n| format!("line {n}\n")).collect()
}

#[test]
fn pre_boundary_lines_are_baseline() {
    let fixture = FixtureRepo::new();
    fixture.write("src/a.rs", &lines(0..10));
    fixture.commit(&ana(), &day(1), "old work");
    fixture.write("src/a.rs", &lines(0..10).replace("line 3\n", "LINE 3\n"));
    let edit = fixture.commit(&luis(), &day(15), "edit old line");

    let since = jiff::Timestamp::from_second(1_768_039_200).unwrap(); // 2026-01-10T10:00Z
    let replay = replay_with(&fixture, &options(since.as_second(), &[]));
    assert_eq!(
        replay.commits.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![oid(&edit)],
        "should replay only commits after the boundary"
    );
    assert_eq!(
        replay.removals,
        vec![pair(None, &edit, 1)],
        "should attribute removed pre-boundary lines to the baseline"
    );
}

#[test]
fn excluded_paths_produce_no_events() {
    let fixture = FixtureRepo::new();
    fixture.write("Cargo.lock", &lines(0..50));
    fixture.write("mix.lock", &lines(0..5));
    fixture.commit(&ana(), &day(1), "locks");
    fixture.write("Cargo.lock", &lines(10..60));
    let bump = fixture.commit(&luis(), &day(2), "bump");

    let replay = replay_all(&fixture);
    assert!(
        files_of(&replay, &bump).is_empty(),
        "should record no file changes for lockfiles"
    );
    assert!(
        replay.removals.is_empty(),
        "should record no removals for lockfiles"
    );
}

#[test]
fn line_replaced_by_another_developer() {
    let fixture = FixtureRepo::new();
    fixture.write("src/a.rs", "one\ntwo\nthree\n");
    let intro = fixture.commit(&ana(), &day(1), "add");
    fixture.write("src/a.rs", "one\nTWO\nthree\n");
    let edit = fixture.commit(&luis(), &day(10), "edit");

    let replay = replay_all(&fixture);
    assert_eq!(
        files_of(&replay, &intro),
        &[FileChange {
            path: "src/a.rs".into(),
            old_path: None,
            added: 3,
            removed: 0,
            new_work: 3
        }],
        "should record three introduced lines for Ana"
    );
    assert_eq!(
        files_of(&replay, &edit),
        &[FileChange {
            path: "src/a.rs".into(),
            old_path: None,
            added: 1,
            removed: 1,
            new_work: 0
        }],
        "should record the replacement as one removed and one added line with no new work"
    );
    assert_eq!(
        replay.removals,
        vec![pair(Some(&intro), &edit, 1)],
        "should link Ana's introducing commit to Luis's removing commit"
    );
}

#[test]
fn line_deleted_by_its_own_author() {
    let fixture = FixtureRepo::new();
    fixture.write("src/a.rs", "keep\ndrop\n");
    let intro = fixture.commit(&ana(), &day(1), "add");
    fixture.write("src/a.rs", "keep\n");
    let delete = fixture.commit(&ana(), &day(4), "delete");

    let replay = replay_all(&fixture);
    assert_eq!(
        replay.removals,
        vec![pair(Some(&intro), &delete, 1)],
        "should record the self-deletion as a removal by the same author's commit"
    );
}

#[test]
fn reindented_line_is_not_a_rewrite() {
    let fixture = FixtureRepo::new();
    fixture.write("src/a.rs", "fn a() {\nbody();\n}\n");
    fixture.commit(&ana(), &day(1), "add");
    fixture.write("src/a.rs", "fn a() {\n    body();\n}\n");
    let reindent = fixture.commit(&luis(), &day(2), "indent");
    fixture.write("src/a.rs", "fn a() {\n    other();\n}\n");
    let edit = fixture.commit(&luis(), &day(3), "edit");

    let replay = replay_all(&fixture);
    assert!(
        files_of(&replay, &reindent).is_empty(),
        "should record no change for indentation only"
    );
    let intro = replay.commits[0].id.to_string();
    assert_eq!(
        replay.removals,
        vec![pair(Some(&intro), &edit, 1)],
        "should keep Ana as the origin of the reindented line"
    );
}

#[test]
fn bot_authored_line_rewritten_by_a_person_keeps_the_bot_as_introducer() {
    let fixture = FixtureRepo::new();
    fixture.write("config/versions.txt", "serde 1.0\n");
    let bot = fixture.commit(
        &Author::new(
            "depbot[bot]",
            "12345678+depbot[bot]@users.noreply.github.com",
        ),
        &day(1),
        "bump",
    );
    fixture.write("config/versions.txt", "serde 1.1\n");
    let edit = fixture.commit(&ana(), &day(2), "edit");

    let replay = replay_all(&fixture);
    assert_eq!(
        replay.removals,
        vec![pair(Some(&bot), &edit, 1)],
        "should record the bot commit as introducer, so metrics can exclude it"
    );
}

#[test]
fn rename_without_edits_keeps_origins() {
    let fixture = FixtureRepo::new();
    fixture.write("src/a.rs", &lines(0..10));
    let intro = fixture.commit(&ana(), &day(1), "add");
    fixture.rename("src/a.rs", "src/b.rs");
    let rename = fixture.commit(&luis(), &day(2), "rename");
    fixture.write("src/b.rs", &lines(0..10).replace("line 5\n", "LINE 5\n"));
    let edit = fixture.commit(&luis(), &day(3), "edit");

    let replay = replay_all(&fixture);
    assert!(
        files_of(&replay, &rename).is_empty(),
        "should record no events for a pure rename"
    );
    assert_eq!(
        replay.removals,
        vec![pair(Some(&intro), &edit, 1)],
        "should keep the original author and date across the rename"
    );
}

#[test]
fn rename_with_small_edits_counts_only_the_edit() {
    let fixture = FixtureRepo::new();
    fixture.write("src/a.rs", &lines(0..20));
    let intro = fixture.commit(&ana(), &day(1), "add");
    fixture.rename("src/a.rs", "src/b.rs");
    fixture.write("src/b.rs", &lines(0..20).replace("line 7\n", "LINE 7\n"));
    let rename = fixture.commit(&luis(), &day(2), "rename and edit");

    let replay = replay_all(&fixture);
    assert_eq!(
        files_of(&replay, &rename),
        &[FileChange {
            path: "src/b.rs".into(),
            old_path: Some("src/a.rs".into()),
            added: 1,
            removed: 1,
            new_work: 0
        }],
        "should diff the renamed file against its source"
    );
    assert_eq!(
        replay.removals,
        vec![pair(Some(&intro), &rename, 1)],
        "should remove only the edited line"
    );
}

#[test]
fn clean_merge_keeps_origins_from_both_parents() {
    let fixture = FixtureRepo::new();
    fixture.write("src/a.rs", "a\nb\nc\n");
    fixture.commit(&ana(), &day(1), "base");
    fixture.branch("feature");
    fixture.write("src/a.rs", "a\nb\nC\n");
    let feature = fixture.commit(&luis(), &day(2), "feature edit");
    fixture.checkout("main");
    fixture.write("src/a.rs", "A\nb\nc\n");
    fixture.commit(&ana(), &day(3), "main edit");
    let merge = fixture.merge("feature", &ana(), &day(4), "merge", &[]);
    fixture.write("src/a.rs", "A\nb\nCC\n");
    let carla = Author::new("Carla", "carla@example.com");
    let edit = fixture.commit(&carla, &day(5), "edit feature line");

    let replay = replay_all(&fixture);
    assert!(
        files_of(&replay, &merge).is_empty(),
        "should record no events for a clean merge"
    );
    assert!(
        replay.removals.contains(&pair(Some(&feature), &edit, 1)),
        "should recover the feature branch origin through the merge, got {:?}",
        replay.removals
    );
    assert!(
        replay.removals.iter().all(|p| p.removed != oid(&merge)),
        "should attribute no removals to a clean merge"
    );
}

#[test]
fn conflict_resolution_is_attributed_to_the_merge_and_double_deletion_counts_once() {
    let fixture = FixtureRepo::new();
    fixture.write("src/a.rs", "x\n");
    let base = fixture.commit(&ana(), &day(1), "base");
    fixture.branch("feature");
    fixture.write("src/a.rs", "y\n");
    let feature = fixture.commit(&luis(), &day(2), "feature");
    fixture.checkout("main");
    fixture.write("src/a.rs", "z\n");
    let main_edit = fixture.commit(&ana(), &day(3), "main");
    let merge = fixture.merge("feature", &ana(), &day(4), "merge", &[("src/a.rs", "w\n")]);

    let replay = replay_all(&fixture);
    assert_eq!(
        files_of(&replay, &merge),
        &[FileChange {
            path: "src/a.rs".into(),
            old_path: None,
            added: 1,
            removed: 2,
            new_work: 0
        }],
        "should attribute the resolved line to the merge and remove both sides"
    );
    let mut expected = vec![
        pair(Some(&base), &feature, 1),
        pair(Some(&feature), &merge, 1),
        pair(Some(&main_edit), &merge, 1),
    ];
    expected.sort();
    assert_eq!(
        replay.removals, expected,
        "should count the base line once, removed by the earlier branch commit"
    );
}

#[test]
fn formatter_commit_in_ignore_revs_file_keeps_authorship() {
    let fixture = FixtureRepo::new();
    fixture.write("src/a.rs", "let a = 'x';\nlet b = 'y';\n");
    let intro = fixture.commit(&ana(), &day(1), "add");
    fixture.write("src/a.rs", "let a = \"x\";\nlet b = \"y\";\n");
    let format = fixture.commit(&luis(), &day(2), "format");
    fixture.write(
        ".git-blame-ignore-revs",
        &format!("# formatter\n{format}\n"),
    );
    fixture.commit(&luis(), &day(3), "ignore formatter");
    fixture.write("src/a.rs", "let a = \"x\";\nlet b = \"z\";\n");
    let edit = fixture.commit(&luis(), &day(4), "edit");

    let replay = replay_all(&fixture);
    let formatter = replay
        .commits
        .iter()
        .find(|c| c.id == oid(&format))
        .unwrap();
    assert!(
        formatter.is_ignored,
        "should mark the listed commit as ignored"
    );
    assert!(
        formatter.files.is_empty(),
        "should record no changes for the ignored commit"
    );
    assert_eq!(
        replay.removals,
        vec![pair(Some(&intro), &edit, 1)],
        "should keep Ana as origin of lines reformatted by the ignored commit"
    );
}

#[test]
fn formatter_commit_in_config_keeps_authorship() {
    let fixture = FixtureRepo::new();
    fixture.write("src/a.rs", "a;\nb;\n");
    let intro = fixture.commit(&ana(), &day(1), "add");
    fixture.write("src/a.rs", "a ;\nb ;\n// formatted\n");
    let format = fixture.commit(&luis(), &day(2), "format");
    fixture.write("src/a.rs", "a ;\nB ;\n// formatted\n");
    let edit = fixture.commit(&luis(), &day(3), "edit");

    let replay = replay_with(&fixture, &options(0, std::slice::from_ref(&format)));
    assert!(
        files_of(&replay, &format).is_empty(),
        "should record no changes for a commit ignored in config"
    );
    assert_eq!(
        replay.removals,
        vec![pair(Some(&intro), &edit, 1)],
        "should keep prior authorship across the ignored commit"
    );
}

#[test]
fn replay_is_read_only() {
    let fixture = FixtureRepo::new();
    fixture.write("src/a.rs", "a\n");
    fixture.commit(&ana(), &day(1), "add");
    fixture.branch("other");
    fixture.write("src/a.rs", "b\n");
    fixture.commit(&luis(), &day(2), "other");
    fixture.checkout("main");
    fixture.write("src/a.rs", "uncommitted\n");
    fixture.write("src/new.rs", "untracked\n");
    fixture.git(&["add", "src/new.rs"]);

    let snapshot = || {
        (
            fixture.git(&["show-ref"]),
            fixture.git(&["status", "--porcelain"]),
            fs::read(fixture.path().join(".git/index")).unwrap(),
            fs::read_to_string(fixture.file_path("src/a.rs")).unwrap(),
        )
    };
    let before = snapshot();
    replay_all(&fixture);
    assert_eq!(
        snapshot(),
        before,
        "should leave refs, index and working tree unchanged"
    );
}

#[test]
fn replay_is_deterministic() {
    let fixture = FixtureRepo::new();
    fixture.write("src/a.rs", &lines(0..30));
    fixture.commit(&ana(), &day(1), "add");
    fixture.branch("feature");
    fixture.write("src/a.rs", &lines(0..30).replace("line 2\n", "x\n"));
    fixture.commit(&luis(), &day(2), "feature");
    fixture.checkout("main");
    fixture.write("src/a.rs", &lines(0..30).replace("line 20\n", "y\n"));
    fixture.commit(&ana(), &day(3), "main");
    fixture.merge("feature", &ana(), &day(4), "merge", &[]);

    let first = replay_all(&fixture);
    let second = replay_all(&fixture);
    assert_eq!(
        first.commits, second.commits,
        "should replay commits identically"
    );
    assert_eq!(
        first.removals, second.removals,
        "should produce the same removals"
    );
    let ids: HashSet<_> = first.commits.iter().map(|c| c.id).collect();
    assert_eq!(ids.len(), 4, "should replay every commit once");
}

fn removed_by(replay: &Replay, sha: &str) -> u64 {
    replay
        .removals
        .iter()
        .filter(|p| p.removed == oid(sha))
        .map(|p| p.lines)
        .sum()
}

#[test]
fn merge_that_takes_one_side_of_twice_committed_content_removes_nothing() {
    let fixture = FixtureRepo::new();
    fixture.write("README.md", "hi\n");
    fixture.commit(&ana(), &day(1), "base");
    fixture.branch("copy");
    fixture.write("doc.md", &lines(0..20));
    fixture.commit(&ana(), &day(2), "add doc on the copy");
    let edited = lines(0..20).replace("line 5\n", "LINE 5\n");
    fixture.write("doc.md", &edited);
    fixture.commit(&ana(), &day(2), "edit one line on the copy");
    fixture.checkout("main");
    fixture.write("doc.md", &lines(0..20));
    fixture.commit(&ana(), &day(3), "add the same doc on main");
    let merge = fixture.merge("copy", &ana(), &day(4), "merge", &[("doc.md", &edited)]);
    fixture.write("doc.md", &edited.replace("line 9\n", "LINE 9\n"));
    let later = fixture.commit(&luis(), &day(5), "edit after merge");

    let replay = replay_all(&fixture);
    assert!(
        files_of(&replay, &merge).is_empty(),
        "should record no changes for a merge whose result is one parent's file"
    );
    assert_eq!(
        removed_by(&replay, &merge),
        0,
        "should not charge the merge with the other parent's copy of the lines"
    );
    assert_eq!(
        removed_by(&replay, &later),
        1,
        "should still count the rewrite that follows the merge"
    );
}

#[test]
fn rename_on_a_branch_survives_a_merge_commit() {
    let fixture = FixtureRepo::new();
    fixture.write("a.txt", &lines(0..20));
    fixture.write("other.txt", "x\n");
    let base = fixture.commit(&ana(), &day(1), "base");
    fixture.branch("feature");
    fixture.rename("a.txt", "b.txt");
    fixture.commit(&luis(), &day(2), "rename");
    fixture.checkout("main");
    fixture.write("other.txt", "y\n");
    fixture.commit(&ana(), &day(3), "main edit");
    let merge = fixture.merge("feature", &ana(), &day(4), "merge", &[]);
    fixture.write("b.txt", &lines(0..20).replace("line 5\n", "LINE 5\n"));
    let later = fixture.commit(&luis(), &day(5), "edit after merge");

    let replay = replay_all(&fixture);
    assert_eq!(
        removed_by(&replay, &merge),
        0,
        "should not charge the merge with the renamed file"
    );
    assert!(
        replay.removals.contains(&pair(Some(&base), &later, 1)),
        "should keep the original origin through the rename and the merge, got {:?}",
        replay.removals
    );
}

#[test]
fn identical_files_before_the_boundary_keep_separate_lines() {
    let fixture = FixtureRepo::new();
    fixture.write("dev/a.txt", &lines(0..10));
    fixture.write("prod/a.txt", &lines(0..10));
    fixture.commit(&ana(), &day(1), "old");
    let edited = lines(0..10).replace("line 3\n", "LINE 3\n");
    fixture.write("dev/a.txt", &edited);
    let dev = fixture.commit(&ana(), &day(15), "edit dev");
    fixture.write("prod/a.txt", &edited);
    let prod = fixture.commit(&luis(), &day(16), "edit prod");

    // 2026-01-10T10:00:00Z, between the old commit and the edits.
    let replay = replay_with(&fixture, &options(1_768_039_200, &[]));
    let mut expected = vec![pair(None, &dev, 1), pair(None, &prod, 1)];
    expected.sort();
    assert_eq!(
        replay.removals, expected,
        "should count one baseline line per file even when both files are the same blob"
    );
}

#[test]
fn file_added_by_an_ignored_revision_has_no_introducer() {
    let fixture = FixtureRepo::new();
    fixture.write("src/a.rs", "a\n");
    fixture.commit(&ana(), &day(1), "base");
    fixture.write("src/generated.rs", "one\ntwo\n");
    let bulk = fixture.commit(&luis(), &day(2), "bulk import");
    fixture.write("src/generated.rs", "one\nTWO\n");
    let edit = fixture.commit(&ana(), &day(3), "edit imported line");

    let replay = replay_with(&fixture, &options(0, &[bulk]));
    assert_eq!(
        replay.removals,
        vec![pair(None, &edit, 1)],
        "should treat lines added by an ignored revision as baseline"
    );
}
