//! `repository-ingestion` pull request scenarios against a mock GraphQL server.

use std::cell::RefCell;
use std::fs;

use jiff::Timestamp;
use kawa::config::{Config, Repository};
use kawa::github_sync::{self, GithubSync, NO_TOKEN_MESSAGE, PrOutcome};
use kawa_store::Store;
use mockito::{Matcher, Server};
use serde_json::{Value, json};

const TOKEN: &str = "ghp_kawaTestTokenThatMustNeverBeStored";

fn now() -> Timestamp {
    "2026-11-10T12:00:00Z".parse().unwrap()
}

fn config() -> Config {
    Config {
        repositories: vec![Repository {
            path: "/src/app".into(),
            branch: Some("main".into()),
            github: Some("acme/app".into()),
        }],
        ..Config::default()
    }
}

fn pr(number: i64, title: &str, updated_at: &str) -> Value {
    json!({
        "id": format!("PR_{number}"), "number": number, "title": title,
        "url": format!("https://github.com/acme/app/pull/{number}"),
        "state": "MERGED", "isDraft": false,
        "createdAt": "2026-11-01T09:00:00Z", "updatedAt": updated_at,
        "mergedAt": updated_at, "closedAt": updated_at,
        "author": { "__typename": "User", "login": "ana-gh" },
        "commits": { "nodes": [
            { "commit": { "oid": format!("{number:040x}"), "authoredDate": "2026-11-01T08:00:00Z",
                          "committedDate": "2026-11-01T08:00:00Z",
                          "author": { "name": "Ana", "email": "ana@example.com", "user": { "login": "ana-gh" } } } }
        ] },
        "reviews": { "nodes": [
            { "id": format!("R_{number}"), "state": "APPROVED", "submittedAt": "2026-11-02T10:00:00Z",
              "author": { "__typename": "User", "login": "luis-gh" },
              "comments": { "nodes": [
                  { "id": format!("RC_{number}"), "createdAt": "2026-11-02T09:55:00Z",
                    "author": { "__typename": "User", "login": "luis-gh" } } ] } },
            { "id": format!("RP_{number}"), "state": "PENDING", "submittedAt": null,
              "author": { "__typename": "User", "login": "carla-gh" }, "comments": { "nodes": [] } }
        ] },
        "comments": { "nodes": [
            { "id": format!("C_{number}"), "createdAt": "2026-11-01T12:00:00Z",
              "author": { "__typename": "Bot", "login": "sonarcloud" } } ] },
        "timelineItems": { "nodes": [
            { "__typename": "ReadyForReviewEvent", "createdAt": "2026-11-01T10:00:00Z",
              "actor": { "__typename": "User", "login": "ana-gh" } },
            { "__typename": "ReviewRequestedEvent", "createdAt": "2026-11-01T10:01:00Z",
              "actor": { "__typename": "User", "login": "ana-gh" }, "requestedReviewer": { "login": "luis-gh" } },
            { "__typename": "MergedEvent", "createdAt": updated_at,
              "actor": { "__typename": "User", "login": "ana-gh" } }
        ] }
    })
}

fn page(prs: Vec<Value>, next: Option<&str>, remaining: i64) -> String {
    json!({ "data": {
        "rateLimit": { "remaining": remaining, "resetAt": "2026-11-10T13:00:00Z" },
        "repository": { "pullRequests": {
            "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next },
            "nodes": prs
        } }
    } })
    .to_string()
}

fn sync_with(
    store: &mut Store,
    server: &Server,
    token: Option<&str>,
    wait: impl FnMut(Timestamp) -> bool + 'static,
) -> Vec<PrOutcome> {
    let mut options = GithubSync {
        token: token.map(str::to_owned),
        api_url: format!("{}/graphql", server.url()),
        wait: Box::new(wait),
    };
    github_sync::sync_pull_requests(store, &config(), now(), &mut options).unwrap()
}

fn cursor(value: &str) -> Matcher {
    Matcher::PartialJson(json!({ "variables": { "cursor": value } }))
}

fn first_page() -> Matcher {
    Matcher::PartialJson(json!({ "variables": { "cursor": null } }))
}

/// Full page of 25 PRs so pagination continues.
fn full_page(start: i64, updated_at: &str) -> Vec<Value> {
    (start..start + 25)
        .map(|n| pr(n, &format!("PR {n}"), updated_at))
        .collect()
}

#[test]
fn first_sync_stores_prs_with_children_and_stops_at_the_window() {
    let mut server = Server::new();
    let p1 = server
        .mock("POST", "/graphql")
        .match_header("authorization", format!("Bearer {TOKEN}").as_str())
        .match_body(first_page())
        .with_body(page(full_page(1, "2026-11-09T00:00:00Z"), Some("c1"), 4000))
        .create();
    let p2 = server
        .mock("POST", "/graphql")
        .match_body(cursor("c1"))
        .with_body(page(
            vec![
                pr(100, "recent", "2026-11-08T00:00:00Z"),
                pr(101, "too old", "2026-01-01T00:00:00Z"),
            ],
            Some("c2"),
            4000,
        ))
        .create();
    let p3 = server
        .mock("POST", "/graphql")
        .match_body(cursor("c2"))
        .expect(0)
        .create();

    let mut store = Store::open_in_memory().unwrap();
    let outcomes = sync_with(&mut store, &server, Some(TOKEN), |_| true);
    p1.assert();
    p2.assert();
    p3.assert();

    assert_eq!(
        outcomes,
        vec![PrOutcome::Synced {
            path: "/src/app".into(),
            pull_requests: 26
        }],
        "should store the 26 PRs updated inside the 180-day window"
    );
    let (prs, commits, reviews, comments, events) = store.pull_request_counts(1).unwrap();
    assert_eq!(prs, 26, "should skip the PR updated before the window");
    assert_eq!(commits, 26, "should store one commit per PR");
    assert_eq!(
        reviews, 26,
        "should store submitted reviews and skip pending ones"
    );
    assert_eq!(
        comments, 52,
        "should store conversation and inline review comments"
    );
    assert_eq!(events, 78, "should store ready, request and merged events");
    assert_eq!(
        store.sync_status(1).unwrap().pr_high_water_mark,
        Some("2026-11-09T00:00:00Z".parse().unwrap()),
        "should record the latest updatedAt as high-water mark"
    );
}

#[test]
fn incremental_sync_replaces_updated_prs() {
    let mut server = Server::new();
    let mut store = Store::open_in_memory().unwrap();
    let first = server
        .mock("POST", "/graphql")
        .with_body(page(
            vec![pr(7, "old title", "2026-11-05T00:00:00Z")],
            None,
            4000,
        ))
        .create();
    sync_with(&mut store, &server, Some(TOKEN), |_| true);
    first.remove();

    server
        .mock("POST", "/graphql")
        .with_body(page(
            vec![
                pr(7, "new title", "2026-11-09T00:00:00Z"),
                pr(6, "unchanged", "2026-11-04T00:00:00Z"),
            ],
            None,
            4000,
        ))
        .create();
    sync_with(&mut store, &server, Some(TOKEN), |_| true);

    assert_eq!(
        store.pull_request_titles(1).unwrap(),
        vec![(7, "new title".to_owned())],
        "should replace PRs updated since the high-water mark and skip older ones"
    );
    let (_, commits, reviews, comments, events) = store.pull_request_counts(1).unwrap();
    assert_eq!(
        (commits, reviews, comments, events),
        (1, 1, 2, 3),
        "should replace children instead of duplicating them"
    );
}

#[test]
fn no_token_skips_pr_sync_with_instructions() {
    let server = Server::new();
    let mut store = Store::open_in_memory().unwrap();
    let outcomes = sync_with(&mut store, &server, None, |_| true);
    assert_eq!(
        outcomes,
        vec![PrOutcome::Skipped {
            path: "/src/app".into(),
            reason: NO_TOKEN_MESSAGE.into()
        }],
        "should skip PR sync and say how to provide a token"
    );
    assert!(
        NO_TOKEN_MESSAGE.contains("GITHUB_TOKEN"),
        "should mention GITHUB_TOKEN"
    );
    assert!(
        NO_TOKEN_MESSAGE.contains("gh auth login"),
        "should mention gh auth login"
    );
}

#[test]
fn rate_limit_waits_or_stops_and_resumes_from_the_high_water_mark() {
    let mut server = Server::new();
    let mut store = Store::open_in_memory().unwrap();

    let p1 = server
        .mock("POST", "/graphql")
        .match_body(first_page())
        .with_body(page(full_page(1, "2026-11-09T00:00:00Z"), Some("c1"), 0))
        .expect(2)
        .create();
    let waits = std::rc::Rc::new(RefCell::new(Vec::new()));
    let recorded = waits.clone();
    let outcomes = sync_with(&mut store, &server, Some(TOKEN), move |reset| {
        recorded.borrow_mut().push(reset);
        false
    });
    assert_eq!(
        waits.borrow().as_slice(),
        &["2026-11-10T13:00:00Z".parse::<Timestamp>().unwrap()],
        "should report the reset time when the budget is exhausted"
    );
    assert!(
        matches!(&outcomes[0], PrOutcome::Failed { error, .. } if error.contains("rate limit")),
        "should stop with a rate limit error under --no-wait, got {outcomes:?}"
    );
    assert_eq!(
        store.pull_request_counts(1).unwrap().0,
        25,
        "should keep the page already fetched"
    );
    let status = store.sync_status(1).unwrap();
    assert_eq!(
        status.pr_high_water_mark, None,
        "should not advance the high-water mark"
    );
    assert!(status.error.is_some(), "should record the failure");

    let p2 = server
        .mock("POST", "/graphql")
        .match_body(cursor("c1"))
        .with_body(page(
            vec![pr(30, "last", "2026-11-08T00:00:00Z")],
            None,
            4000,
        ))
        .create();
    let outcomes = sync_with(&mut store, &server, Some(TOKEN), |_| true);
    p1.assert();
    p2.assert();
    assert_eq!(
        outcomes,
        vec![PrOutcome::Synced {
            path: "/src/app".into(),
            pull_requests: 26
        }],
        "should resume from the window start and finish after waiting"
    );
}

#[test]
fn http_403_with_spent_budget_is_a_rate_limit() {
    let mut server = Server::new();
    server
        .mock("POST", "/graphql")
        .with_status(403)
        .with_header("x-ratelimit-remaining", "0")
        .with_header("x-ratelimit-reset", "1794312000")
        .with_body("{\"message\":\"API rate limit exceeded\"}")
        .create();
    let mut store = Store::open_in_memory().unwrap();
    let waited = std::rc::Rc::new(RefCell::new(None));
    let seen = waited.clone();
    let outcomes = sync_with(&mut store, &server, Some(TOKEN), move |reset| {
        *seen.borrow_mut() = Some(reset);
        false
    });
    assert_eq!(
        *waited.borrow(),
        Some(Timestamp::from_second(1_794_312_000).unwrap()),
        "should read the reset time from the headers"
    );
    assert!(
        matches!(outcomes[0], PrOutcome::Failed { .. }),
        "should fail without waiting"
    );
}

#[test]
fn token_is_never_stored_or_reported() {
    let mut server = Server::new();
    server
        .mock("POST", "/graphql")
        .with_status(401)
        .with_body(format!("{{\"message\":\"Bad credentials for {TOKEN}\"}}"))
        .create();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kawa.sqlite");
    let mut store = Store::open(&path).unwrap();
    let outcomes = sync_with(&mut store, &server, Some(TOKEN), |_| true);
    drop(store);

    let reported = format!("{outcomes:?}");
    assert!(
        reported.contains("401"),
        "should report the failure status, got {reported}"
    );
    assert!(
        !reported.contains(TOKEN),
        "should redact the token from errors"
    );
    for entry in fs::read_dir(dir.path()).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        assert!(
            !bytes.windows(TOKEN.len()).any(|w| w == TOKEN.as_bytes()),
            "should never write the token to the database files"
        );
    }
}

#[test]
fn bot_actors_get_the_bot_suffix() {
    let page = kawa_github::parse_page(&page(vec![pr(1, "t", "2026-11-09T00:00:00Z")], None, 4000))
        .unwrap();
    assert_eq!(
        page.pull_requests[0].comments[0].author_login, "sonarcloud[bot]",
        "should mark Bot actors so the bot pattern excludes them"
    );
}

#[test]
fn longer_window_fetches_again_from_its_start() {
    let mut server = Server::new();
    let mut store = Store::open_in_memory().unwrap();
    let sync = |store: &mut Store, server: &Server, days: u32| {
        let mut options = GithubSync {
            token: Some(TOKEN.to_owned()),
            api_url: format!("{}/graphql", server.url()),
            wait: Box::new(|_| true),
        };
        let config = Config {
            sync_window_days: days,
            ..config()
        };
        github_sync::sync_pull_requests(store, &config, now(), &mut options).unwrap()
    };
    let pages = || {
        page(
            vec![
                pr(7, "recent", "2026-11-09T00:00:00Z"),
                pr(6, "two weeks old", "2026-10-27T00:00:00Z"),
            ],
            None,
            4000,
        )
    };

    let first = server.mock("POST", "/graphql").with_body(pages()).create();
    sync(&mut store, &server, 7);
    first.remove();
    assert_eq!(
        store.pull_request_titles(1).unwrap(),
        vec![(7, "recent".to_owned())],
        "should stop at the 7-day window on the first sync"
    );

    let second = server.mock("POST", "/graphql").with_body(pages()).create();
    sync(&mut store, &server, 30);
    second.remove();
    assert_eq!(
        store.pull_request_titles(1).unwrap().len(),
        2,
        "should fetch the older pull request once the window grows past the last sync"
    );
    let covered = store.sync_status(1).unwrap().pr_window_start;
    assert_eq!(
        covered,
        Some("2026-10-11T12:00:00Z".parse().unwrap()),
        "should remember the start of the longer window"
    );

    let third = server
        .mock("POST", "/graphql")
        .with_body(page(
            vec![pr(6, "must not be read again", "2026-10-27T00:00:00Z")],
            None,
            4000,
        ))
        .create();
    sync(&mut store, &server, 30);
    third.remove();
    assert!(
        store
            .pull_request_titles(1)
            .unwrap()
            .contains(&(6, "two weeks old".to_owned())),
        "should continue from the last sync while the window is covered"
    );
}
