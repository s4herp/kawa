//! Spec scenarios of `developer-identity` (git side) on fixture repositories.

use std::collections::BTreeMap;

use kawa_core::identity::{DeveloperId, DeveloperSpec, DisplayNames, IdentityRules, Resolution};
use kawa_core::repo;
use kawa_testkit::{Author, FixtureRepo};

const BOT: &str = r"(?i)\[bot\]$";

struct Outcome {
    subjects_by_developer: BTreeMap<String, Vec<String>>,
    bot_commits: Vec<String>,
    names: DisplayNames,
}

/// Resolves every commit author on `main` the way sync will: mailmap from the
/// branch tip, then config rules.
fn resolve_history(fixture: &FixtureRepo, developers: &[DeveloperSpec]) -> Outcome {
    let repository = repo::open(fixture.path()).unwrap();
    let tip = repo::branch_tip(&repository, Some("main")).unwrap();
    let mailmap = repo::mailmap_at(&tip).unwrap();
    let mailmaps: Vec<&[u8]> = mailmap.as_deref().into_iter().collect();
    let rules = IdentityRules::new(&mailmaps, developers, BOT).unwrap();

    let mut outcome = Outcome {
        subjects_by_developer: BTreeMap::new(),
        bot_commits: Vec::new(),
        names: DisplayNames::new(developers),
    };
    for info in repository.rev_walk([tip.id]).all().unwrap() {
        let commit = info.unwrap().object().unwrap();
        let author = commit.author().unwrap();
        let subject = commit.message().unwrap().summary().to_string();
        match rules.resolve_author(&author.name.to_string(), &author.email.to_string()) {
            Resolution::Developer { id, name } => {
                outcome.names.observe(&id, &name, 1);
                outcome
                    .subjects_by_developer
                    .entry(id.as_str().to_owned())
                    .or_default()
                    .push(subject);
            }
            Resolution::Bot => outcome.bot_commits.push(subject),
        }
    }
    for subjects in outcome.subjects_by_developer.values_mut() {
        subjects.sort();
    }
    outcome
}

fn ids(outcome: &Outcome) -> Vec<&str> {
    outcome
        .subjects_by_developer
        .keys()
        .map(String::as_str)
        .collect()
}

#[test]
fn two_emails_mapped_to_one_person() {
    let fixture = FixtureRepo::new();
    fixture.write(".mailmap", "Ana <ana@new.example> <ana@old.example>\n");
    fixture.commit(
        &Author::new("Ana", "ana@old.example"),
        "2026-01-05T09:00:00+00:00",
        "old email",
    );
    fixture.write("a.txt", "a\n");
    fixture.commit(
        &Author::new("Ana", "ana@new.example"),
        "2026-01-06T09:00:00+00:00",
        "new email",
    );

    let outcome = resolve_history(&fixture, &[]);
    assert_eq!(
        ids(&outcome),
        vec!["ana@new.example"],
        "should attribute commits from both emails to one developer"
    );
    assert_eq!(
        outcome.subjects_by_developer["ana@new.example"],
        vec!["new email", "old email"],
        "should keep every commit of both emails"
    );
}

#[test]
fn alias_not_present_in_mailmap() {
    let fixture = FixtureRepo::new();
    fixture.write("a.txt", "a\n");
    fixture.commit(
        &Author::new("Ana", "ana@personal.example"),
        "2026-01-05T09:00:00+00:00",
        "personal",
    );
    fixture.write("a.txt", "b\n");
    fixture.commit(
        &Author::new("Ana", "ana@new.example"),
        "2026-01-06T09:00:00+00:00",
        "work",
    );

    let developers = [DeveloperSpec {
        email: "ana@new.example".into(),
        aliases: vec!["ana@personal.example".into()],
        ..Default::default()
    }];
    let outcome = resolve_history(&fixture, &developers);
    assert_eq!(
        ids(&outcome),
        vec!["ana@new.example"],
        "should apply config aliases when the mailmap does not cover them"
    );
}

#[test]
fn config_absent_uses_mailmap_only() {
    let fixture = FixtureRepo::new();
    fixture.write(".mailmap", "Ana <ana@new.example> <ana@old.example>\n");
    fixture.commit(
        &Author::new("Ana", "ana@old.example"),
        "2026-01-05T09:00:00+00:00",
        "a",
    );
    fixture.write("a.txt", "a\n");
    fixture.commit(
        &Author::new("Luis", "luis@example.com"),
        "2026-01-06T09:00:00+00:00",
        "b",
    );

    let outcome = resolve_history(&fixture, &[]);
    assert_eq!(
        ids(&outcome),
        vec!["ana@new.example", "luis@example.com"],
        "should resolve with the mailmap alone and keep unmapped authors as they are"
    );
}

#[test]
fn name_spelled_two_ways_with_same_email() {
    let fixture = FixtureRepo::new();
    let spellings = [
        ("Ana Pérez", "ana@x.example"),
        ("ana perez", "ANA@x.example"),
        ("Ana Pérez", "Ana@X.example"),
    ];
    for (index, (name, email)) in spellings.iter().enumerate() {
        fixture.write("a.txt", &format!("{index}\n"));
        fixture.commit(
            &Author::new(name, email),
            &format!("2026-01-0{}T09:00:00+00:00", index + 5),
            &format!("c{index}"),
        );
    }

    let outcome = resolve_history(&fixture, &[]);
    assert_eq!(
        ids(&outcome),
        vec!["ana@x.example"],
        "should treat emails that differ only in case as one developer"
    );
    assert_eq!(
        outcome
            .names
            .name(&DeveloperId::from_email("ana@x.example")),
        "Ana Pérez",
        "should display the more frequent of the two names"
    );
}

#[test]
fn depbot_commits_are_excluded() {
    let fixture = FixtureRepo::new();
    fixture.write("Cargo.lock", "a\n");
    fixture.commit(
        &Author::new(
            "depbot[bot]",
            "12345678+depbot[bot]@users.noreply.github.com",
        ),
        "2026-01-05T09:00:00+00:00",
        "bump serde",
    );
    fixture.write("a.txt", "a\n");
    fixture.commit(
        &Author::new("Ana", "ana@x.example"),
        "2026-01-06T09:00:00+00:00",
        "feature",
    );

    let outcome = resolve_history(&fixture, &[]);
    assert_eq!(
        ids(&outcome),
        vec!["ana@x.example"],
        "should attribute no commits to the bot"
    );
    assert_eq!(
        outcome.bot_commits,
        vec!["bump serde"],
        "should report the bot commit as excluded"
    );
}

#[test]
fn mailmap_is_read_from_the_branch_not_the_working_tree() {
    let fixture = FixtureRepo::new();
    fixture.write(".mailmap", "Ana <ana@new.example> <ana@old.example>\n");
    fixture.commit(
        &Author::new("Ana", "ana@old.example"),
        "2026-01-05T09:00:00+00:00",
        "a",
    );
    fixture.write(".mailmap", "Other <other@example.com> <ana@old.example>\n");

    let outcome = resolve_history(&fixture, &[]);
    assert_eq!(
        ids(&outcome),
        vec!["ana@new.example"],
        "should ignore uncommitted mailmap edits in the working tree"
    );
}
