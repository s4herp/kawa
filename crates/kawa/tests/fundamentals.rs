//! `code-fundamentals` scenarios, end to end: fixture repositories, sync,
//! refresh and the fundamentals query.

use std::collections::BTreeSet;

use jiff::Timestamp;
use jiff::civil::Date;
use kawa::config::{Config, Repository};
use kawa::sync;
use kawa_store::{DeveloperFundamentals, Selection, Store, WorkType, WorkTypes};
use kawa_testkit::{Author, FixtureRepo};

const NOW: &str = "2026-03-01T12:00:00Z";

fn ana() -> Author {
    Author::new("Ana", "ana@example.com")
}

fn luis() -> Author {
    Author::new("Luis", "luis@example.com")
}

fn config(repos: &[&FixtureRepo], timezone: &str) -> Config {
    Config {
        timezone: Some(timezone.to_owned()),
        sync_window_days: 30,
        min_lines: 1,
        repositories: repos
            .iter()
            .map(|r| Repository {
                path: r.path().to_owned(),
                branch: Some("main".into()),
                github: None,
            })
            .collect(),
        ..Config::default()
    }
}

fn synced(config: &Config) -> Store {
    let mut store = Store::open_in_memory().unwrap();
    sync::sync_git(&mut store, config, NOW.parse::<Timestamp>().unwrap()).unwrap();
    sync::refresh(&mut store, config).unwrap();
    store
}

fn selection(start: &str, end: &str) -> Selection {
    Selection {
        start: start.parse::<Date>().unwrap(),
        end: end.parse::<Date>().unwrap(),
        repositories: BTreeSet::new(),
        developers: BTreeSet::new(),
    }
}

fn row<'a>(rows: &'a [DeveloperFundamentals], id: &str) -> &'a DeveloperFundamentals {
    rows.iter()
        .find(|r| r.developer_id == id)
        .unwrap_or_else(|| panic!("no fundamentals for {id}: {rows:?}"))
}

#[test]
fn commit_near_midnight_counts_toward_the_local_day() {
    let fixture = FixtureRepo::new();
    fixture.write("a.txt", "a\n");
    fixture.commit(&ana(), "2026-02-10T02:30:00+00:00", "late night");
    let config = config(&[&fixture], "America/Santiago");
    let store = synced(&config);

    let rows = store
        .fundamentals(
            &selection("2026-02-09", "2026-02-09"),
            &config.impact_settings(),
        )
        .unwrap();
    assert_eq!(
        row(&rows, "ana@example.com").active_days,
        1,
        "should count 23:30 Santiago time on the earlier day"
    );
    let next_day = store
        .fundamentals(
            &selection("2026-02-10", "2026-02-10"),
            &config.impact_settings(),
        )
        .unwrap();
    assert!(
        next_day.is_empty(),
        "should not count the commit on the UTC day"
    );
}

#[test]
fn active_days_and_commits_per_active_day_span_repositories() {
    let first = FixtureRepo::new();
    let second = FixtureRepo::new();
    for (repo, hour) in [(&first, 9), (&first, 10), (&second, 11), (&second, 12)] {
        repo.write("a.txt", &format!("{hour}\n"));
        repo.commit(&ana(), &format!("2026-02-16T{hour}:00:00+00:00"), "monday");
    }
    first.write("a.txt", "wednesday\n");
    first.commit(&ana(), "2026-02-18T10:00:00+00:00", "wednesday");
    let config = config(&[&first, &second], "UTC");
    let store = synced(&config);

    let rows = store
        .fundamentals(
            &selection("2026-02-16", "2026-02-22"),
            &config.impact_settings(),
        )
        .unwrap();
    let ana = row(&rows, "ana@example.com");
    assert_eq!(
        ana.active_days, 2,
        "should count Monday and Wednesday once each"
    );
    assert_eq!(
        ana.commits, 5,
        "should count commits from both repositories"
    );
    assert_eq!(
        ana.commits_per_active_day, 2.5,
        "should divide commits by active days"
    );
}

/// Ana introduces lines and they are later changed by Ana (churn), by Luis
/// (helping others), after 21 days (legacy) and from before the boundary
/// (legacy); a bot-introduced line rewritten by Ana is legacy too.
fn work_type_fixture() -> FixtureRepo {
    let fixture = FixtureRepo::new();
    fixture.write("old.txt", "old 1\nold 2\n");
    fixture.commit(&ana(), "2025-11-01T10:00:00+00:00", "before the boundary");
    fixture.write("bot.txt", "pinned 1\n");
    fixture.commit(
        &Author::new("refresher[bot]", "bot@example.com"),
        "2026-02-01T10:00:00+00:00",
        "bot line",
    );
    fixture.write("a.txt", "a1\na2\na3\na4\n");
    fixture.commit(&ana(), "2026-02-01T10:00:00+00:00", "ana adds");
    fixture.write("a.txt", "A1\na2\na3\na4\n");
    fixture.commit(
        &ana(),
        "2026-02-05T10:00:00+00:00",
        "ana rewrites her own line",
    );
    fixture.write("a.txt", "A1\nL2\na3\na4\n");
    fixture.commit(
        &luis(),
        "2026-02-06T10:00:00+00:00",
        "luis rewrites ana's line",
    );
    fixture.write("a.txt", "A1\nL2\nA3\na4\n");
    fixture.write("old.txt", "OLD 1\nold 2\n");
    fixture.write("bot.txt", "pinned 2\n");
    fixture.commit(
        &ana(),
        "2026-02-25T10:00:00+00:00",
        "ana rewrites old lines",
    );
    fixture
}

#[test]
fn removed_lines_are_classified_by_origin_and_age() {
    let fixture = work_type_fixture();
    let config = config(&[&fixture], "UTC");
    let store = synced(&config);
    let rows = store
        .fundamentals(
            &selection("2026-02-01", "2026-02-28"),
            &config.impact_settings(),
        )
        .unwrap();

    assert_eq!(
        row(&rows, "ana@example.com").work,
        WorkTypes {
            new_work: 4,
            legacy_refactor: 3,
            help_others: 0,
            churn: 1
        },
        "should count Ana's own recent rewrite as churn, and the 24-day-old, \
         pre-boundary and bot lines as legacy refactor"
    );
    assert_eq!(
        row(&rows, "luis@example.com").work,
        WorkTypes {
            new_work: 0,
            legacy_refactor: 0,
            help_others: 1,
            churn: 0
        },
        "should count Luis rewriting Ana's recent line as helping others"
    );
    assert!(
        rows.iter().all(|r| !r.developer_id.contains("bot")),
        "should give the bot no fundamentals"
    );
}

#[test]
fn pure_insertion_beyond_removed_lines_is_new_work() {
    let fixture = FixtureRepo::new();
    let base: String = (0..10).map(|n| format!("base {n}\n")).collect();
    fixture.write("a.txt", &base);
    fixture.commit(&luis(), "2025-12-01T10:00:00+00:00", "old");
    let replacement: String = (0..30).map(|n| format!("new {n}\n")).collect();
    fixture.write("a.txt", &replacement);
    fixture.commit(&ana(), "2026-02-10T10:00:00+00:00", "replace 10 with 30");
    let config = config(&[&fixture], "UTC");
    let store = synced(&config);

    let rows = store
        .fundamentals(
            &selection("2026-02-01", "2026-02-28"),
            &config.impact_settings(),
        )
        .unwrap();
    let work = row(&rows, "ana@example.com").work;
    assert_eq!(
        work.legacy_refactor, 10,
        "should classify the 10 replaced lines by origin"
    );
    assert_eq!(
        work.new_work, 20,
        "should count the 20 lines beyond them as new work"
    );
}

#[test]
fn efficiency_impact_and_insufficient_data() {
    let fixture = work_type_fixture();
    let mut config = config(&[&fixture], "UTC");
    let store = synced(&config);
    let rows = store
        .fundamentals(
            &selection("2026-02-01", "2026-02-28"),
            &config.impact_settings(),
        )
        .unwrap();
    let ana = row(&rows, "ana@example.com");
    assert_eq!(
        ana.efficiency,
        Some(1.0 - 1.0 / 8.0),
        "should exclude churn from efficiency"
    );
    assert_eq!(ana.files, 3, "should count distinct files touched");
    assert_eq!(ana.modules, 1, "should count root files as one module");
    assert_eq!(
        ana.impact,
        Some(1.0),
        "should rank highest on every input against Luis and weigh them to 1.0"
    );

    config.min_lines = 5;
    let rows = store
        .fundamentals(
            &selection("2026-02-01", "2026-02-28"),
            &config.impact_settings(),
        )
        .unwrap();
    let luis = row(&rows, "luis@example.com");
    assert_eq!(
        (luis.efficiency, luis.impact),
        (None, None),
        "should show insufficient data below the minimum changed lines"
    );
}

#[test]
fn evidence_sums_to_every_shown_value() {
    let fixture = work_type_fixture();
    let config = config(&[&fixture], "UTC");
    let store = synced(&config);
    let selection = selection("2026-02-01", "2026-02-28");
    let rows = store
        .fundamentals(&selection, &config.impact_settings())
        .unwrap();

    for developer in &rows {
        let evidence = store
            .fundamentals_evidence(&selection, &developer.developer_id, config.recency_days)
            .unwrap();
        let mut total = WorkTypes::default();
        for commit in &evidence {
            total.new_work += commit.work.new_work;
            total.legacy_refactor += commit.work.legacy_refactor;
            total.help_others += commit.work.help_others;
            total.churn += commit.work.churn;
            let rewritten: u64 = commit.rewrote.iter().map(|r| r.lines).sum();
            assert_eq!(
                rewritten,
                commit.work.legacy_refactor + commit.work.help_others + commit.work.churn,
                "should list the rewritten commits behind {}",
                commit.subject
            );
        }
        assert_eq!(
            total, developer.work,
            "should sum per-commit counts to the shown values for {}",
            developer.name
        );
    }

    let ana = store
        .fundamentals_evidence(&selection, "ana@example.com", config.recency_days)
        .unwrap();
    let churn_commit = ana
        .iter()
        .find(|c| c.subject == "ana rewrites her own line")
        .unwrap();
    assert_eq!(
        churn_commit.rewrote[0].work_type,
        WorkType::Churn,
        "should label the rewritten lines with their work type"
    );
    assert_eq!(
        churn_commit.rewrote[0].introduced_by.as_deref(),
        Some("ana@example.com"),
        "should name who introduced the rewritten lines"
    );
}
