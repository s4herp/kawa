//! `web-dashboard` and `work-log` scenarios on rendered pages.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use jiff::Timestamp;
use kawa::config::{Config, Repository};
use kawa::sync;
use kawa::web::{self, AppState};
use kawa_github::{Comment, CommentKind, PrCommit, PrState, PullRequest, Review};
use kawa_store::{Store, SyncErrorKind};
use kawa_testkit::{Author, FixtureRepo};
use tower::ServiceExt;

const NOW: &str = "2026-11-10T12:00:00Z";

struct World {
    _fixture: FixtureRepo,
    config: Config,
    store: Store,
}

fn author(name: &str) -> Author {
    Author::new(name, &format!("{}@example.com", name.to_lowercase()))
}

fn pr(number: i64, login: &str, reviewer: Option<&str>) -> PullRequest {
    let at = |v: &str| v.parse::<Timestamp>().unwrap();
    PullRequest {
        node_id: format!("PR_{number}"),
        number,
        title: format!("Change {number}"),
        url: format!("https://github.com/acme/app/pull/{number}"),
        author_login: login.into(),
        state: PrState::Merged,
        is_draft: false,
        created_at: at("2026-11-03T10:00:00Z"),
        updated_at: at("2026-11-04T16:00:00Z"),
        merged_at: Some(at("2026-11-04T16:00:00Z")),
        closed_at: Some(at("2026-11-04T16:00:00Z")),
        commits: vec![PrCommit {
            sha: format!("{number:040}"),
            author_login: Some(login.into()),
            author_name: login.into(),
            author_email: format!("{login}@example.com"),
            authored_at: at("2026-11-03T09:00:00Z"),
            committed_at: at("2026-11-03T09:00:00Z"),
        }],
        reviews: reviewer
            .map(|r| {
                vec![Review {
                    node_id: format!("R{number}"),
                    author_login: r.into(),
                    state: "APPROVED".into(),
                    submitted_at: at("2026-11-04T10:00:00Z"),
                }]
            })
            .unwrap_or_default(),
        comments: vec![Comment {
            node_id: format!("C{number}"),
            author_login: "luis".into(),
            kind: CommentKind::Issue,
            created_at: at("2026-11-04T09:00:00Z"),
        }]
        .into_iter()
        .filter(|_| reviewer.is_some())
        .collect(),
        events: Vec::new(),
    }
}

fn world(min_lines: u64) -> World {
    let fixture = FixtureRepo::new();
    let lines: String = (0..60).map(|n| format!("line {n}\n")).collect();
    fixture.write("apps/core/a.rs", &lines);
    fixture.commit(&author("Ana"), "2026-11-02T10:00:00+00:00", "ana adds core");
    fixture.write("apps/core/a.rs", &lines.replace("line 5\n", "LINE 5\n"));
    fixture.commit(
        &author("Ana"),
        "2026-11-03T10:00:00+00:00",
        "ana fixes her line",
    );
    fixture.write("apps/web/b.rs", &lines);
    fixture.commit(
        &author("Luis"),
        "2026-11-04T10:00:00+00:00",
        "luis adds web",
    );
    fixture.write("README.md", "hello\n");
    fixture.commit(&author("Carla"), "2026-11-07T10:00:00+00:00", "carla docs");
    let config = Config {
        timezone: Some("UTC".into()),
        min_lines,
        repositories: vec![Repository {
            path: fixture.path().to_owned(),
            branch: Some("main".into()),
            github: Some("acme/app".into()),
        }],
        ..Config::default()
    };
    let mut store = Store::open_in_memory().unwrap();
    sync::sync_git(&mut store, &config, NOW.parse().unwrap()).unwrap();
    store
        .replace_pull_requests(1, &[pr(1, "ana", Some("luis")), pr(2, "luis", None)])
        .unwrap();
    sync::refresh(&mut store, &config).unwrap();
    World {
        _fixture: fixture,
        config,
        store,
    }
}

fn build_app(world: World, now: &str) -> (axum::Router, FixtureRepo) {
    let now: Timestamp = now.parse().unwrap();
    let state = Arc::new(AppState {
        store: Mutex::new(world.store),
        config: world.config,
        clock: Arc::new(move || now),
    });
    (web::router(state), world._fixture)
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

const PAGES: &[&str] = &[
    "/",
    "/evidence?id=ana%40example.com&metric=churn",
    "/developer?id=ana%40example.com",
    "/work-log",
    "/work-log/day?id=ana%40example.com&day=2026-11-03",
    "/reviews",
    "/reviews/prs?author=ana%40example.com",
    "/pr?repository=1&number=1",
    "/identities",
    "/sync",
];

#[tokio::test]
async fn every_page_renders_without_external_resources() {
    let (app, _keep) = build_app(world(1), NOW);
    for path in PAGES {
        let (status, body) = get(&app, path).await;
        assert_eq!(status, StatusCode::OK, "should render {path}");
        for forbidden in ["<script", "<link", "src=\"http", "@import", "url(http"] {
            assert!(
                !body.contains(forbidden),
                "should load nothing external on {path}: found {forbidden}"
            );
        }
    }
}

#[tokio::test]
async fn navigation_reaches_every_view() {
    let (app, _keep) = build_app(world(1), NOW);
    let (_, body) = get(&app, "/").await;
    for (href, label) in [
        ("/", "Fundamentals"),
        ("/work-log", "Work log"),
        ("/reviews", "Reviews"),
        ("/identities", "Identities"),
        ("/sync", "Sync"),
    ] {
        assert!(
            body.contains(&format!("href=\"{href}")),
            "should link {label} from the navigation"
        );
    }
}

#[tokio::test]
async fn filtered_url_renders_the_same_view_again() {
    let (app, _keep) = build_app(world(1), NOW);
    let path = "/?period=custom&start=2026-11-01&end=2026-11-05&repo=1&dev=ana%40example.com";
    let (_, first) = get(&app, path).await;
    let (_, again) = get(&app, path).await;
    assert_eq!(
        first, again,
        "should render identical pages for the same URL"
    );
    assert!(
        first.contains("value=\"2026-11-01\""),
        "should keep the custom start in the form"
    );
    assert!(
        first.contains("name=\"dev\" value=\"ana@example.com\" checked"),
        "should keep the developer filter"
    );
    let table = first.split("<tbody>").nth(1).expect("table rendered");
    assert!(
        table.contains(">Ana<") && !table.contains(">Luis<"),
        "should leave unselected developers out of the table"
    );
    assert!(
        first.contains("dev=ana%40example.com"),
        "should carry the filters into every link"
    );
}

#[tokio::test]
async fn overview_shows_quadrant_without_ranking() {
    let (app, _keep) = build_app(world(10), NOW);
    let (_, body) = get(&app, "/?period=30").await;
    assert!(body.contains("<svg"), "should draw the quadrant");
    assert!(
        body.contains("Cambios amplios, poco churn"),
        "should label quadrants as patterns"
    );
    assert!(
        body.contains("no es una evaluación de desempeño"),
        "should state the chart is not an evaluation"
    );
    assert!(
        body.contains("datos insuficientes") && body.contains("Carla"),
        "should list Carla below the chart"
    );
    let (ana, carla, luis) = (
        body.find(">Ana<").unwrap(),
        body.find(">Carla<").unwrap(),
        body.find(">Luis<").unwrap(),
    );
    assert!(
        ana < carla && carla < luis,
        "should order developers alphabetically"
    );
    assert!(
        !body.to_lowercase().contains("rank"),
        "should have no rank column"
    );
}

#[tokio::test]
async fn developer_profile_shows_weeks_and_pull_requests() {
    let (app, _keep) = build_app(world(1), NOW);
    let (_, body) = get(&app, "/developer?id=ana%40example.com").await;
    assert!(
        body.contains("Tipos de trabajo por semana"),
        "should show the weekly trend"
    );
    assert!(
        body.contains("Semana del") && body.contains("2026-11-02"),
        "should start weeks on Monday"
    );
    assert!(
        body.contains("Tiempo de ciclo"),
        "should show Ana's PR metrics"
    );
}

#[tokio::test]
async fn work_log_grid_and_cells() {
    let (app, _keep) = build_app(world(1), NOW);
    let (_, body) = get(
        &app,
        "/work-log?period=custom&start=2026-11-02&end=2026-11-15",
    )
    .await;
    let header = body.split("<tbody>").next().unwrap();
    assert_eq!(
        header.matches("<th>").count() + header.matches("<th class").count() - 1,
        14,
        "should have one column per day of the 14-day period"
    );
    assert!(header.contains("class=\"weekend\""), "should mark weekends");
    let rows = body
        .split("<tbody>")
        .nth(1)
        .unwrap()
        .split("</tbody>")
        .next()
        .unwrap();
    assert_eq!(
        rows.matches("<tr>").count(),
        3,
        "should have one row per developer with activity"
    );
    assert!(
        rows.contains("</i>C1</span><span class=\"chip\"><i class=\"s2\"></i>A1</span>"),
        "should count Ana's commit and opened PR on the same day"
    );

    let (_, cell) = get(&app, "/work-log/day?id=ana%40example.com&day=2026-11-03").await;
    assert!(
        cell.contains("https://github.com/acme/app/commit/"),
        "should link the commit on GitHub"
    );
    assert!(
        cell.contains("https://github.com/acme/app/pull/1"),
        "should link the opened PR"
    );
    assert!(
        cell.contains("churn 1"),
        "should show the commit's work types"
    );

    let (_, empty) = get(&app, "/work-log/day?id=carla%40example.com&day=2026-11-03").await;
    assert!(
        !empty.contains("Commit</td>"),
        "should list nothing for a day without activity"
    );

    let (_, long) = get(
        &app,
        "/work-log?period=custom&start=2026-06-01&end=2026-10-01",
    )
    .await;
    assert!(
        long.contains("Elige un período más corto"),
        "should ask for a shorter period"
    );
    assert!(
        !long.contains("class=\"grid\""),
        "should not render a partial grid"
    );
}

#[tokio::test]
async fn review_view_matrix_and_pr_timeline() {
    let (app, _keep) = build_app(world(1), NOW);
    let (_, body) = get(&app, "/reviews").await;
    assert!(
        body.contains("Quién revisa a quién"),
        "should show the reviewer-author matrix"
    );
    assert!(
        body.contains("/reviews/prs?reviewer=luis%40example.com&amp;author=ana%40example.com"),
        "should link the matrix cell to its PRs"
    );
    let (_, prs) = get(
        &app,
        "/reviews/prs?reviewer=luis%40example.com&author=ana%40example.com",
    )
    .await;
    assert!(
        prs.contains("Change 1") && !prs.contains("Change 2"),
        "should list only PRs Luis reviewed for Ana"
    );

    let (_, detail) = get(&app, "/pr?repository=1&number=1").await;
    assert!(
        detail.contains("Eventos usados"),
        "should show the timeline"
    );
    assert!(
        detail.contains("revisión") && detail.contains("luis"),
        "should list the review with its actor"
    );
    assert!(
        detail.contains("Espera"),
        "should show the computed timings"
    );
}

#[tokio::test]
async fn identities_and_sync_views() {
    let (app, _keep) = build_app(world(1), NOW);
    let (_, identities) = get(&app, "/identities").await;
    assert!(
        identities.contains("ana &lt;ana@example.com&gt;")
            || identities.contains("ana <ana@example.com>")
            || identities.contains("&lt;ana@example.com&gt;"),
        "should list raw identities"
    );
    let (_, sync) = get(&app, "/sync").await;
    assert!(
        sync.contains("acme/app") && sync.contains(NOW.trim_end_matches('Z')),
        "should show the last sync"
    );
    assert!(
        !sync.contains("Los datos pueden estar desactualizados"),
        "should not warn about fresh data"
    );
}

#[tokio::test]
async fn stale_and_failed_syncs_are_flagged() {
    let (app, _keep) = build_app(world(1), "2026-11-13T12:00:00Z");
    let (_, body) = get(&app, "/").await;
    assert!(
        body.contains("Los datos pueden estar desactualizados"),
        "should warn when the last sync is 3 days old"
    );
    assert!(
        body.contains("último sync: 2026-11-10"),
        "should name the sync time"
    );

    let w = world(1);
    w.store
        .record_sync_error(
            1,
            SyncErrorKind::Github,
            NOW.parse().unwrap(),
            "rate limited",
        )
        .unwrap();
    let (app, _keep) = build_app(w, NOW);
    let (_, body) = get(&app, "/reviews").await;
    assert!(
        body.contains("el último sync falló") && body.contains("rate limited"),
        "should flag the failed sync"
    );
}

#[tokio::test]
async fn every_metric_value_links_to_its_evidence() {
    let (app, _keep) = build_app(world(1), NOW);
    for path in [
        "/",
        "/developer?id=ana%40example.com",
        "/reviews",
        "/work-log",
    ] {
        let (_, body) = get(&app, path).await;
        let mut checked = 0;
        for cell in body.split("<td class=\"num\">").skip(1) {
            let content = cell.split("</td>").next().unwrap().trim();
            if content.is_empty() {
                continue;
            }
            assert!(
                content.starts_with("<a class=\"metric\" href=\""),
                "should link every value on {path}, found {content}"
            );
            checked += 1;
        }
        for cell in body
            .split("<td class=\"weekend\">")
            .skip(1)
            .chain(body.split("<td>").skip(1).filter(|_| path == "/work-log"))
        {
            let content = cell.split("</td>").next().unwrap().trim();
            if content.is_empty() || content.starts_with("<a ") {
                continue;
            }
            panic!("should link every work log cell, found {content}");
        }
        assert!(
            checked > 0 || path == "/work-log",
            "should check some values on {path}"
        );
    }
}
