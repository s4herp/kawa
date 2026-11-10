//! Read-only GitHub GraphQL client for pull request data.

use std::time::Duration;

use jiff::Timestamp;
use serde_json::json;
use thiserror::Error;

pub const DEFAULT_API_URL: &str = "https://api.github.com/graphql";
const PAGE_SIZE: u32 = 25;
/// Waiting stops while this much budget is left, so a full page always fits.
const MIN_REMAINING: i64 = 100;

#[derive(Debug, Error)]
pub enum GithubError {
    #[error("GitHub request failed: {0}")]
    Http(String),
    #[error("GitHub returned status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("GitHub GraphQL errors: {0}")]
    Graphql(String),
    #[error("unexpected GitHub response: {0}")]
    Decode(String),
    #[error("GitHub rate limit exhausted until {reset_at}")]
    RateLimited { reset_at: Timestamp },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub node_id: String,
    pub number: i64,
    pub title: String,
    pub url: String,
    pub author_login: String,
    pub state: PrState,
    pub is_draft: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub merged_at: Option<Timestamp>,
    pub closed_at: Option<Timestamp>,
    pub commits: Vec<PrCommit>,
    pub reviews: Vec<Review>,
    pub comments: Vec<Comment>,
    pub events: Vec<TimelineEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

impl PrState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Merged => "merged",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCommit {
    pub sha: String,
    pub author_login: Option<String>,
    pub author_name: String,
    pub author_email: String,
    pub authored_at: Timestamp,
    pub committed_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub node_id: String,
    pub author_login: String,
    pub state: String,
    pub submitted_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentKind {
    /// A comment on the pull request conversation.
    Issue,
    /// An inline comment that belongs to a review.
    Review,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub node_id: String,
    pub author_login: String,
    pub kind: CommentKind,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEvent {
    pub kind: EventKind,
    pub actor_login: Option<String>,
    /// The requested reviewer for `ReviewRequested`.
    pub subject_login: Option<String>,
    pub at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    ReadyForReview,
    ConvertToDraft,
    ReviewRequested,
    Merged,
    ForcePushed,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadyForReview => "ready_for_review",
            Self::ConvertToDraft => "convert_to_draft",
            Self::ReviewRequested => "review_requested",
            Self::Merged => "merged",
            Self::ForcePushed => "force_pushed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    pub remaining: i64,
    pub reset_at: Timestamp,
}

/// One page of pull requests, newest update first.
#[derive(Debug, Clone)]
pub struct Page {
    pub pull_requests: Vec<PullRequest>,
    pub next_cursor: Option<String>,
    pub rate_limit: RateLimit,
}

pub struct Client {
    http: reqwest::blocking::Client,
    url: String,
    token: String,
}

impl Client {
    pub fn new(token: String, url: &str) -> Result<Self, GithubError> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("kawa/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| GithubError::Http(e.to_string()))?;
        Ok(Self {
            http,
            url: url.to_owned(),
            token,
        })
    }

    /// Fetches one page of pull requests of `owner/name` ordered by last update.
    pub fn page(&self, owner: &str, name: &str, cursor: Option<&str>) -> Result<Page, GithubError> {
        let body = json!({
            "query": QUERY,
            "variables": { "owner": owner, "name": name, "cursor": cursor, "pageSize": PAGE_SIZE },
        });
        let response = self
            .http
            .post(&self.url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .map_err(|e| GithubError::Http(redact(&e.to_string(), &self.token)))?;
        let status = response.status();
        let headers = response.headers().clone();
        let text = response
            .text()
            .map_err(|e| GithubError::Http(redact(&e.to_string(), &self.token)))?;
        if (status.as_u16() == 403 || status.as_u16() == 429)
            && let Some(reset_at) = reset_from_headers(&headers)
        {
            return Err(GithubError::RateLimited { reset_at });
        }
        if !status.is_success() {
            return Err(GithubError::Status {
                status: status.as_u16(),
                body: redact(&text, &self.token),
            });
        }
        parse_page(&text)
    }
}

/// Fetches every page of pull requests updated at or after `since`, calling
/// `on_page` with each page. When the budget runs out, `wait` decides: it
/// sleeps until the reset and returns `true` to continue, or returns `false`
/// to stop with `RateLimited`.
pub fn fetch_updated_since(
    client: &Client,
    owner: &str,
    name: &str,
    since: Timestamp,
    mut on_page: impl FnMut(&[PullRequest]) -> Result<(), GithubError>,
    mut wait: impl FnMut(Timestamp) -> bool,
) -> Result<(), GithubError> {
    let mut cursor: Option<String> = None;
    loop {
        let page = match client.page(owner, name, cursor.as_deref()) {
            Err(GithubError::RateLimited { reset_at }) => {
                if wait(reset_at) {
                    continue;
                }
                return Err(GithubError::RateLimited { reset_at });
            }
            other => other?,
        };
        let in_window: Vec<PullRequest> = page
            .pull_requests
            .into_iter()
            .filter(|pr| pr.updated_at >= since)
            .collect();
        let reached_older = in_window.len() < PAGE_SIZE as usize;
        on_page(&in_window)?;
        match page.next_cursor {
            Some(next) if !reached_older => cursor = Some(next),
            _ => return Ok(()),
        }
        if page.rate_limit.remaining < MIN_REMAINING && !wait(page.rate_limit.reset_at) {
            return Err(GithubError::RateLimited {
                reset_at: page.rate_limit.reset_at,
            });
        }
    }
}

fn reset_from_headers(headers: &reqwest::header::HeaderMap) -> Option<Timestamp> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    // A 403 is a rate limit only when the budget is spent or GitHub asks to retry.
    let exhausted = header("x-ratelimit-remaining") == Some("0");
    if exhausted
        && let Some(reset) = header("x-ratelimit-reset").and_then(|v| v.parse::<i64>().ok())
    {
        return Timestamp::from_second(reset).ok();
    }
    let retry_after = header("retry-after").and_then(|v| v.parse::<i64>().ok())?;
    Timestamp::now()
        .checked_add(jiff::SignedDuration::from_secs(retry_after))
        .ok()
}

fn redact(text: &str, token: &str) -> String {
    if token.is_empty() {
        text.to_owned()
    } else {
        text.replace(token, "[redacted]")
    }
}

pub fn parse_page(text: &str) -> Result<Page, GithubError> {
    let response: wire::Response =
        serde_json::from_str(text).map_err(|e| GithubError::Decode(e.to_string()))?;
    if let Some(errors) = response.errors.filter(|e| !e.is_empty()) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        return Err(GithubError::Graphql(messages.join("; ")));
    }
    let data = response
        .data
        .ok_or_else(|| GithubError::Decode("missing data".into()))?;
    let repository = data
        .repository
        .ok_or_else(|| GithubError::Graphql("repository not found".into()))?;
    let connection = repository.pull_requests;
    Ok(Page {
        pull_requests: connection
            .nodes
            .into_iter()
            .map(wire::PullRequest::into_model)
            .collect(),
        next_cursor: connection
            .page_info
            .has_next_page
            .then_some(connection.page_info.end_cursor)
            .flatten(),
        rate_limit: RateLimit {
            remaining: data.rate_limit.remaining,
            reset_at: data.rate_limit.reset_at,
        },
    })
}

const QUERY: &str = r#"
query($owner: String!, $name: String!, $cursor: String, $pageSize: Int!) {
  rateLimit { remaining resetAt }
  repository(owner: $owner, name: $name) {
    pullRequests(first: $pageSize, after: $cursor, orderBy: {field: UPDATED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        id number title url state isDraft createdAt updatedAt mergedAt closedAt
        author { __typename login }
        commits(first: 100) {
          nodes { commit { oid authoredDate committedDate author { name email user { login } } } }
        }
        reviews(first: 100) {
          nodes {
            id state submittedAt author { __typename login }
            comments(first: 100) { nodes { id createdAt author { __typename login } } }
          }
        }
        comments(first: 100) { nodes { id createdAt author { __typename login } } }
        timelineItems(first: 100, itemTypes: [READY_FOR_REVIEW_EVENT, CONVERT_TO_DRAFT_EVENT,
                                              REVIEW_REQUESTED_EVENT, MERGED_EVENT, HEAD_REF_FORCE_PUSHED_EVENT]) {
          nodes {
            __typename
            ... on ReadyForReviewEvent { createdAt actor { __typename login } }
            ... on ConvertToDraftEvent { createdAt actor { __typename login } }
            ... on ReviewRequestedEvent {
              createdAt actor { __typename login }
              requestedReviewer { ... on User { login } ... on Bot { login } }
            }
            ... on MergedEvent { createdAt actor { __typename login } }
            ... on HeadRefForcePushedEvent { createdAt actor { __typename login } }
          }
        }
      }
    }
  }
}
"#;

mod wire {
    //! Shapes of the GraphQL response.

    use jiff::Timestamp;
    use serde::Deserialize;

    use super::{CommentKind, EventKind, PrState};

    #[derive(Deserialize)]
    pub struct Response {
        pub data: Option<Data>,
        pub errors: Option<Vec<Error>>,
    }

    #[derive(Deserialize)]
    pub struct Error {
        pub message: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Data {
        pub rate_limit: RateLimit,
        pub repository: Option<Repository>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct RateLimit {
        pub remaining: i64,
        pub reset_at: Timestamp,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Repository {
        pub pull_requests: Connection<PullRequest>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Connection<T> {
        #[serde(default = "PageInfo::last")]
        pub page_info: PageInfo,
        pub nodes: Vec<T>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct PageInfo {
        pub has_next_page: bool,
        pub end_cursor: Option<String>,
    }

    impl PageInfo {
        fn last() -> Self {
            Self {
                has_next_page: false,
                end_cursor: None,
            }
        }
    }

    /// A deleted account has no actor; GitHub shows it as "ghost".
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Actor {
        #[serde(rename = "__typename")]
        pub typename: Option<String>,
        pub login: String,
    }

    pub fn login(actor: Option<Actor>) -> String {
        match actor {
            // Bot logins come without the [bot] suffix git and REST use.
            Some(Actor {
                typename: Some(t),
                login,
            }) if t == "Bot" && !login.ends_with("[bot]") => format!("{login}[bot]"),
            Some(actor) => actor.login,
            None => "ghost".to_owned(),
        }
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct PullRequest {
        pub id: String,
        pub number: i64,
        pub title: String,
        pub url: String,
        pub state: String,
        pub is_draft: bool,
        pub created_at: Timestamp,
        pub updated_at: Timestamp,
        pub merged_at: Option<Timestamp>,
        pub closed_at: Option<Timestamp>,
        pub author: Option<Actor>,
        pub commits: Connection<CommitNode>,
        pub reviews: Connection<Review>,
        pub comments: Connection<Comment>,
        pub timeline_items: Connection<TimelineItem>,
    }

    #[derive(Deserialize)]
    pub struct CommitNode {
        pub commit: Commit,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Commit {
        pub oid: String,
        pub authored_date: Timestamp,
        pub committed_date: Timestamp,
        pub author: Option<GitActor>,
    }

    #[derive(Deserialize)]
    pub struct GitActor {
        pub name: Option<String>,
        pub email: Option<String>,
        pub user: Option<UserLogin>,
    }

    #[derive(Deserialize)]
    pub struct UserLogin {
        pub login: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Review {
        pub id: String,
        pub state: String,
        pub submitted_at: Option<Timestamp>,
        pub author: Option<Actor>,
        pub comments: Connection<Comment>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Comment {
        pub id: String,
        pub created_at: Timestamp,
        pub author: Option<Actor>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct TimelineItem {
        #[serde(rename = "__typename")]
        pub typename: String,
        pub created_at: Option<Timestamp>,
        pub actor: Option<Actor>,
        pub requested_reviewer: Option<RequestedReviewer>,
    }

    #[derive(Deserialize)]
    pub struct RequestedReviewer {
        pub login: Option<String>,
    }

    impl PullRequest {
        pub fn into_model(self) -> super::PullRequest {
            let state = match self.state.as_str() {
                "MERGED" => PrState::Merged,
                "CLOSED" => PrState::Closed,
                _ => PrState::Open,
            };
            let mut comments: Vec<super::Comment> = self
                .comments
                .nodes
                .into_iter()
                .map(|c| comment(c, CommentKind::Issue))
                .collect();
            let mut reviews = Vec::new();
            for review in self.reviews.nodes {
                comments.extend(
                    review
                        .comments
                        .nodes
                        .into_iter()
                        .map(|c| comment(c, CommentKind::Review)),
                );
                // Pending reviews have no submission time and are not visible yet.
                if let Some(submitted_at) = review.submitted_at {
                    reviews.push(super::Review {
                        node_id: review.id,
                        author_login: login(review.author),
                        state: review.state,
                        submitted_at,
                    });
                }
            }
            let events = self
                .timeline_items
                .nodes
                .into_iter()
                .filter_map(|item| {
                    let kind = match item.typename.as_str() {
                        "ReadyForReviewEvent" => EventKind::ReadyForReview,
                        "ConvertToDraftEvent" => EventKind::ConvertToDraft,
                        "ReviewRequestedEvent" => EventKind::ReviewRequested,
                        "MergedEvent" => EventKind::Merged,
                        "HeadRefForcePushedEvent" => EventKind::ForcePushed,
                        _ => return None,
                    };
                    Some(super::TimelineEvent {
                        kind,
                        actor_login: item.actor.map(|a| login(Some(a))),
                        subject_login: item.requested_reviewer.and_then(|r| r.login),
                        at: item.created_at?,
                    })
                })
                .collect();
            super::PullRequest {
                node_id: self.id,
                number: self.number,
                title: self.title,
                url: self.url,
                author_login: login(self.author),
                state,
                is_draft: self.is_draft,
                created_at: self.created_at,
                updated_at: self.updated_at,
                merged_at: self.merged_at,
                closed_at: self.closed_at,
                commits: self
                    .commits
                    .nodes
                    .into_iter()
                    .map(|node| {
                        let author = node.commit.author;
                        super::PrCommit {
                            sha: node.commit.oid,
                            author_login: author
                                .as_ref()
                                .and_then(|a| a.user.as_ref())
                                .map(|u| u.login.clone()),
                            author_name: author
                                .as_ref()
                                .and_then(|a| a.name.clone())
                                .unwrap_or_default(),
                            author_email: author.and_then(|a| a.email).unwrap_or_default(),
                            authored_at: node.commit.authored_date,
                            committed_at: node.commit.committed_date,
                        }
                    })
                    .collect(),
                reviews,
                comments,
                events,
            }
        }
    }

    fn comment(c: Comment, kind: CommentKind) -> super::Comment {
        super::Comment {
            node_id: c.id,
            author_login: login(c.author),
            kind,
            created_at: c.created_at,
        }
    }
}
