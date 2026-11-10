//! `review-collaboration` scenarios on stored pull requests.

use std::collections::BTreeSet;

use jiff::Timestamp;
use jiff::civil::Date;
use kawa::config::Config;
use kawa::sync;
use kawa_github::{
    Comment, CommentKind, EventKind, PrCommit, PrState, PullRequest, Review, TimelineEvent,
};
use kawa_store::{Selection, Store};

const HOUR: f64 = 3600.0;

fn at(value: &str) -> Timestamp {
    value.parse().unwrap()
}

/// A merged, non-draft PR by `author` with one commit, created at `ready`.
fn pr(
    number: i64,
    author: &str,
    first_commit: &str,
    ready: &str,
    merged: Option<&str>,
) -> PullRequest {
    PullRequest {
        node_id: format!("PR_{number}"),
        number,
        title: format!("PR {number}"),
        url: format!("https://github.com/acme/app/pull/{number}"),
        author_login: author.into(),
        state: if merged.is_some() {
            PrState::Merged
        } else {
            PrState::Closed
        },
        is_draft: false,
        created_at: at(ready),
        updated_at: at(merged.unwrap_or(ready)),
        merged_at: merged.map(at),
        closed_at: merged.map(at),
        commits: vec![commit(number, 0, author, first_commit)],
        reviews: Vec::new(),
        comments: Vec::new(),
        events: Vec::new(),
    }
}

fn commit(number: i64, index: i64, login: &str, when: &str) -> PrCommit {
    PrCommit {
        sha: format!("{number:020}{index:020}"),
        author_login: Some(login.into()),
        author_name: login.into(),
        author_email: format!("{login}@example.com"),
        authored_at: at(when),
        committed_at: at(when),
    }
}

fn review(id: &str, login: &str, state: &str, when: &str) -> Review {
    Review {
        node_id: id.into(),
        author_login: login.into(),
        state: state.into(),
        submitted_at: at(when),
    }
}

fn comment(id: &str, login: &str, when: &str) -> Comment {
    Comment {
        node_id: id.into(),
        author_login: login.into(),
        kind: CommentKind::Issue,
        created_at: at(when),
    }
}

fn event(kind: EventKind, actor: &str, subject: Option<&str>, when: &str) -> TimelineEvent {
    TimelineEvent {
        kind,
        actor_login: Some(actor.into()),
        subject_login: subject.map(str::to_owned),
        at: at(when),
    }
}

fn store_with(prs: Vec<PullRequest>) -> Store {
    let mut store = Store::open_in_memory().unwrap();
    let repo = store
        .upsert_repository("/src/app", None, Some("acme/app"))
        .unwrap();
    store.replace_pull_requests(repo, &prs).unwrap();
    let config = Config {
        timezone: Some("UTC".into()),
        ..Config::default()
    };
    sync::refresh(&mut store, &config).unwrap();
    store
}

fn november() -> Selection {
    Selection {
        start: "2026-11-01".parse::<Date>().unwrap(),
        end: "2026-11-30".parse::<Date>().unwrap(),
        repositories: BTreeSet::new(),
        developers: BTreeSet::new(),
    }
}

#[test]
fn draft_time_is_excluded_from_the_review_clock() {
    let mut p = pr(
        1,
        "ana",
        "2026-11-01T08:00:00Z",
        "2026-11-01T09:00:00Z",
        Some("2026-11-05T12:00:00Z"),
    );
    p.events = vec![event(
        EventKind::ReadyForReview,
        "ana",
        None,
        "2026-11-04T09:00:00Z",
    )];
    p.reviews = vec![review("R1", "luis", "APPROVED", "2026-11-04T11:00:00Z")];
    let store = store_with(vec![p]);

    let detail = store.pr_detail(1, 1).unwrap().unwrap();
    assert_eq!(
        detail.timings.ready_at,
        Some(at("2026-11-04T09:00:00Z")),
        "should start the review clock when the draft became ready"
    );
    assert_eq!(
        detail.timings.pickup_seconds,
        Some(2 * 3600),
        "should measure pickup from ready"
    );
}

#[test]
fn cycle_time_splits_into_coding_pickup_and_review() {
    let mut p = pr(
        1,
        "ana",
        "2026-11-02T09:00:00Z",
        "2026-11-02T15:00:00Z",
        Some("2026-11-03T16:00:00Z"),
    );
    p.reviews = vec![review("R1", "luis", "APPROVED", "2026-11-03T10:00:00Z")];
    let store = store_with(vec![p]);

    let t = store.pr_detail(1, 1).unwrap().unwrap().timings;
    assert_eq!(
        (
            t.coding_seconds,
            t.pickup_seconds,
            t.review_seconds,
            t.cycle_seconds
        ),
        (
            Some(6 * 3600),
            Some(19 * 3600),
            Some(6 * 3600),
            Some(31 * 3600)
        ),
        "should split 31 h into 6 h coding, 19 h pickup and 6 h review"
    );
}

#[test]
fn merged_without_review_is_unreviewed() {
    let store = store_with(vec![pr(
        1,
        "ana",
        "2026-11-02T09:00:00Z",
        "2026-11-02T10:00:00Z",
        Some("2026-11-02T11:00:00Z"),
    )]);
    let t = store.pr_detail(1, 1).unwrap().unwrap().timings;
    assert_eq!(
        (t.pickup_seconds, t.review_seconds),
        (None, None),
        "should not compute pickup or review"
    );
    assert!(t.is_unreviewed, "should count the PR as unreviewed");

    let summary = store.review_summary(&november()).unwrap();
    let ana = &summary.authors[0];
    assert_eq!(
        (ana.unreviewed, ana.merged),
        (1, 1),
        "should count Ana's self-merged PR"
    );
    assert_eq!(
        ana.unreviewed_share(),
        Some(1.0),
        "should report the unreviewed share"
    );
}

#[test]
fn bot_comments_do_not_start_the_review() {
    let mut p = pr(
        1,
        "ana",
        "2026-11-02T09:00:00Z",
        "2026-11-02T10:00:00Z",
        Some("2026-11-02T18:00:00Z"),
    );
    p.comments = vec![comment("C1", "sonarcloud[bot]", "2026-11-02T10:01:00Z")];
    p.reviews = vec![review("R1", "luis", "COMMENTED", "2026-11-02T13:00:00Z")];
    let store = store_with(vec![p]);
    assert_eq!(
        store
            .pr_detail(1, 1)
            .unwrap()
            .unwrap()
            .timings
            .pickup_seconds,
        Some(3 * 3600),
        "should take time to first comment from the first person, not the bot"
    );
}

#[test]
fn reviewer_reaction_starts_at_the_request() {
    let mut p = pr(
        1,
        "ana",
        "2026-11-02T08:00:00Z",
        "2026-11-02T09:00:00Z",
        Some("2026-11-02T18:00:00Z"),
    );
    p.events = vec![event(
        EventKind::ReviewRequested,
        "ana",
        Some("luis"),
        "2026-11-02T10:00:00Z",
    )];
    p.reviews = vec![review("R1", "luis", "APPROVED", "2026-11-02T13:00:00Z")];
    let store = store_with(vec![p]);

    let summary = store.review_summary(&november()).unwrap();
    let luis = summary
        .reviewers
        .iter()
        .find(|r| r.developer_id == "gh:luis")
        .unwrap();
    assert_eq!(
        luis.reaction.map(|a| a.median),
        Some(3.0 * HOUR),
        "should measure 3 hours from request to review"
    );
}

#[test]
fn author_responsiveness_is_the_time_to_the_next_push() {
    let mut p = pr(
        1,
        "ana",
        "2026-11-02T08:00:00Z",
        "2026-11-02T09:00:00Z",
        Some("2026-11-02T18:00:00Z"),
    );
    p.reviews = vec![review(
        "R1",
        "luis",
        "CHANGES_REQUESTED",
        "2026-11-02T13:00:00Z",
    )];
    p.commits.push(commit(1, 1, "ana", "2026-11-02T15:00:00Z"));
    let store = store_with(vec![p]);

    let summary = store.review_summary(&november()).unwrap();
    assert_eq!(
        summary.authors[0].responsiveness.map(|a| a.median),
        Some(2.0 * HOUR),
        "should measure 2 hours from the review to Ana's next commit"
    );
}

#[test]
fn review_rounds_followed_by_commits_are_iterations() {
    let mut p = pr(
        1,
        "ana",
        "2026-11-02T08:00:00Z",
        "2026-11-02T09:00:00Z",
        Some("2026-11-03T18:00:00Z"),
    );
    p.reviews = vec![
        review("R1", "luis", "CHANGES_REQUESTED", "2026-11-02T10:00:00Z"),
        review("R2", "luis", "CHANGES_REQUESTED", "2026-11-02T14:00:00Z"),
        review("R3", "luis", "APPROVED", "2026-11-03T10:00:00Z"),
    ];
    p.commits.push(commit(1, 1, "ana", "2026-11-02T12:00:00Z"));
    p.commits.push(commit(1, 2, "ana", "2026-11-02T16:00:00Z"));
    let store = store_with(vec![p]);
    assert_eq!(
        store.pr_detail(1, 1).unwrap().unwrap().timings.iterations,
        2,
        "should count two review rounds followed by new commits"
    );
}

#[test]
fn matrix_counts_prs_reviewed_per_author_and_aggregates_medians() {
    let mut prs = Vec::new();
    for n in 1..=12 {
        let mut p = pr(
            n,
            "ana",
            "2026-11-02T08:00:00Z",
            "2026-11-02T09:00:00Z",
            Some("2026-11-04T18:00:00Z"),
        );
        let reviewer = if n <= 7 { "luis" } else { "carla" };
        p.reviews = vec![review(
            &format!("R{n}"),
            reviewer,
            "APPROVED",
            &format!("2026-11-02T{:02}:00:00Z", 9 + n),
        )];
        prs.push(p);
    }
    let store = store_with(prs);
    let summary = store.review_summary(&november()).unwrap();

    let cell = summary
        .matrix
        .iter()
        .find(|c| c.reviewer_id == "gh:luis" && c.author_id == "ana@example.com")
        .unwrap();
    assert_eq!(
        cell.pull_requests.len(),
        7,
        "should count the 7 PRs Luis reviewed for Ana"
    );
    assert_eq!(
        cell.pull_requests[0],
        (1, 1),
        "should list the PRs behind the cell"
    );

    let pickup = summary.authors[0].pickup.unwrap();
    assert_eq!(pickup.n, 12, "should aggregate over Ana's 12 merged PRs");
    assert_eq!(pickup.median, 6.5 * HOUR, "should report the median pickup");
    assert_eq!(
        pickup.p75,
        9.0 * HOUR,
        "should report the 75th percentile pickup"
    );
}

#[test]
fn pr_detail_lists_the_events_used() {
    let mut p = pr(
        1,
        "ana",
        "2026-11-02T09:00:00Z",
        "2026-11-02T15:00:00Z",
        Some("2026-11-03T16:00:00Z"),
    );
    p.events = vec![
        event(
            EventKind::ReviewRequested,
            "ana",
            Some("luis"),
            "2026-11-02T15:05:00Z",
        ),
        event(EventKind::Merged, "ana", None, "2026-11-03T16:00:00Z"),
    ];
    p.reviews = vec![review("R1", "luis", "APPROVED", "2026-11-03T10:00:00Z")];
    let store = store_with(vec![p]);

    let detail = store.pr_detail(1, 1).unwrap().unwrap();
    let kinds: Vec<(&str, Option<&str>)> = detail
        .events
        .iter()
        .map(|e| (e.kind.as_str(), e.actor.as_deref()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("commit", Some("ana")),
            ("review_requested", Some("ana")),
            ("review", Some("luis")),
            ("merged", Some("ana")),
        ],
        "should list each event with its actor in time order"
    );
    assert_eq!(
        detail.timings.cycle_seconds,
        Some(31 * 3600),
        "should show the computed timings"
    );
}
