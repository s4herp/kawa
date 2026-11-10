//! Login linking scenarios and the identity audit, on a seeded database.

use std::fs;
use std::path::Path;

use kawa::identities;
use kawa_core::identity::{DeveloperSpec, IdentityRules};
use kawa_store::Store;

const BOT: &str = r"(?i)\[bot\]$";

fn seeded_store() -> Store {
    let store = Store::open_in_memory().unwrap();
    let repo = store
        .upsert_repository("/src/app", Some("main"), Some("acme/app"))
        .unwrap();
    let ana_old = store.upsert_git_identity("Ana", "ana@old.example").unwrap();
    let ana_new = store
        .upsert_git_identity("Ana P", "ana@new.example")
        .unwrap();
    let bot = store
        .upsert_git_identity(
            "depbot[bot]",
            "12345678+depbot[bot]@users.noreply.github.com",
        )
        .unwrap();
    store
        .execute_batch(&format!(
            "INSERT INTO commits VALUES
               ({repo}, 'c1', {ana_old}, '2026-01-05T09:00:00Z', 0, 'one', 0, 0),
               ({repo}, 'c2', {ana_old}, '2026-01-06T09:00:00Z', 0, 'two', 0, 0),
               ({repo}, 'c3', {ana_new}, '2026-01-07T09:00:00Z', 0, 'three', 0, 0),
               ({repo}, 'c4', {bot}, '2026-01-08T09:00:00Z', 0, 'bump', 0, 0);
             INSERT INTO pull_requests VALUES
               ({repo}, 1, 'PR_1', 'ana-gh', 'Feature', 'https://github.com/acme/app/pull/1', 'merged', 0,
                '2026-01-05T08:00:00Z', '2026-01-07T10:00:00Z', '2026-01-07T10:00:00Z', '2026-01-07T10:00:00Z'),
               ({repo}, 2, 'PR_2', 'depbot[bot]', 'Bump', 'https://github.com/acme/app/pull/2', 'merged', 0,
                '2026-01-08T08:00:00Z', '2026-01-08T10:00:00Z', '2026-01-08T10:00:00Z', '2026-01-08T10:00:00Z');
             INSERT INTO pr_commits VALUES
               ({repo}, 1, 'c1', 'ana-gh', 'ana@old.example', 'Ana', '2026-01-05T09:00:00Z', '2026-01-05T09:00:00Z'),
               ({repo}, 1, 'c2', 'ana-gh', 'ana@old.example', 'Ana', '2026-01-06T09:00:00Z', '2026-01-06T09:00:00Z');
             INSERT INTO reviews VALUES
               ({repo}, 1, 'R_1', 'carla-gh', 'APPROVED', '2026-01-07T09:30:00Z'),
               ({repo}, 2, 'R_2', 'carla-gh', 'APPROVED', '2026-01-08T09:30:00Z');"
        ))
        .unwrap();
    store
}

fn rules(developers: &[DeveloperSpec]) -> IdentityRules {
    let mailmap: &[u8] = b"Ana <ana@new.example> <ana@old.example>\n";
    IdentityRules::new(&[mailmap], developers, BOT).unwrap()
}

fn entry<'a>(entries: &'a [identities::AuditEntry], identity: &str) -> &'a identities::AuditEntry {
    entries
        .iter()
        .find(|e| e.identity == identity)
        .unwrap_or_else(|| panic!("missing identity {identity}"))
}

#[test]
fn login_seen_on_pr_commits_links_to_the_developer() {
    let mut store = seeded_store();
    identities::resolve(&mut store, &rules(&[]), &[]).unwrap();
    let entries = identities::audit(&store).unwrap();
    let login = entry(&entries, "@ana-gh");
    assert_eq!(
        login.developer_id.as_deref(),
        Some("ana@new.example"),
        "should link the login to the developer its PR commits resolve to"
    );
    assert_eq!(
        login.status, "developer",
        "should report a linked login as a developer"
    );
    assert_eq!(
        login.pull_requests, 1,
        "should count PRs authored by the login"
    );
}

#[test]
fn login_with_no_linked_email_is_its_own_developer() {
    let mut store = seeded_store();
    identities::resolve(&mut store, &rules(&[]), &[]).unwrap();
    let entries = identities::audit(&store).unwrap();
    let login = entry(&entries, "@carla-gh");
    assert_eq!(
        login.developer_id.as_deref(),
        Some("gh:carla-gh"),
        "should key a review-only login by the login"
    );
    assert_eq!(
        login.status, "unlinked",
        "should mark it as unlinked in the audit"
    );
    assert_eq!(login.reviews, 2, "should count its reviews");
}

#[test]
fn config_login_link_resolves_a_review_only_login() {
    let mut store = seeded_store();
    let developers = [DeveloperSpec {
        email: "carla@example.com".into(),
        name: Some("Carla".into()),
        logins: vec!["carla-gh".into()],
        ..Default::default()
    }];
    identities::resolve(&mut store, &rules(&developers), &developers).unwrap();
    let entries = identities::audit(&store).unwrap();
    let login = entry(&entries, "@carla-gh");
    assert_eq!(
        (login.developer_id.as_deref(), login.developer.as_deref()),
        (Some("carla@example.com"), Some("Carla")),
        "should apply the config link and configured name"
    );
}

#[test]
fn bot_identities_are_excluded() {
    let mut store = seeded_store();
    identities::resolve(&mut store, &rules(&[]), &[]).unwrap();
    let entries = identities::audit(&store).unwrap();
    for identity in [
        "depbot[bot] <12345678+depbot[bot]@users.noreply.github.com>",
        "@depbot[bot]",
    ] {
        let bot = entry(&entries, identity);
        assert_eq!(bot.status, "excluded: bot", "should exclude {identity}");
        assert_eq!(
            bot.developer_id, None,
            "should not assign a developer to {identity}"
        );
    }
}

#[test]
fn audit_json_matches_golden_file() {
    let mut store = seeded_store();
    identities::resolve(&mut store, &rules(&[]), &[]).unwrap();
    let actual = identities::render_json(identities::audit(&store).unwrap());
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/identities.json");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(golden.parent().unwrap()).unwrap();
        fs::write(&golden, &actual).unwrap();
    }
    let expected =
        fs::read_to_string(&golden).expect("golden file exists; run with UPDATE_GOLDEN=1");
    assert_eq!(
        actual, expected,
        "should render the audit exactly as the golden file"
    );
}

#[test]
fn table_lists_every_identity() {
    let mut store = seeded_store();
    identities::resolve(&mut store, &rules(&[]), &[]).unwrap();
    let entries = identities::audit(&store).unwrap();
    let table = identities::render_table(&entries);
    assert_eq!(
        table.lines().count(),
        entries.len() + 1,
        "should print a header plus one line per identity"
    );
    assert!(
        table.starts_with("IDENTITY"),
        "should start with the header"
    );
}
