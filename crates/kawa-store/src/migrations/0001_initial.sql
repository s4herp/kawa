-- Raw data: what git and GitHub report. Derived tables are rebuilt from these.

CREATE TABLE repositories (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    branch TEXT,
    github TEXT
) STRICT;

CREATE TABLE sync_status (
    repository_id INTEGER PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
    git_synced_at TEXT,
    git_head TEXT,
    pr_synced_at TEXT,
    pr_high_water_mark TEXT,
    error_kind TEXT CHECK (error_kind IN ('git', 'github')),
    error_message TEXT,
    error_at TEXT
) STRICT;

-- Raw identities as they appear in git (name + email) or GitHub (login).
CREATE TABLE identities (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL DEFAULT '',
    email TEXT NOT NULL DEFAULT '',
    login TEXT NOT NULL DEFAULT '',
    developer_id TEXT,
    excluded INTEGER NOT NULL DEFAULT 0 CHECK (excluded IN (0, 1)),
    UNIQUE (name, email, login)
) STRICT;

CREATE INDEX identities_developer_id ON identities (developer_id);

CREATE TABLE commits (
    repository_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    sha TEXT NOT NULL,
    author_identity_id INTEGER NOT NULL REFERENCES identities(id),
    author_at TEXT NOT NULL,
    author_offset_seconds INTEGER NOT NULL,
    subject TEXT NOT NULL,
    is_merge INTEGER NOT NULL CHECK (is_merge IN (0, 1)),
    is_ignored INTEGER NOT NULL CHECK (is_ignored IN (0, 1)),
    PRIMARY KEY (repository_id, sha)
) STRICT;

CREATE INDEX commits_author_at ON commits (author_at);
CREATE INDEX commits_author_identity ON commits (author_identity_id, author_at);

-- Per file and commit: line counts. Added lines that do not replace a removed
-- line in the same hunk are new work; removed lines are classified at query
-- time from line_removals, so recency and identity changes need no replay.
CREATE TABLE commit_files (
    repository_id INTEGER NOT NULL,
    sha TEXT NOT NULL,
    path TEXT NOT NULL,
    old_path TEXT,
    lines_added INTEGER NOT NULL,
    lines_removed INTEGER NOT NULL,
    new_work INTEGER NOT NULL,
    PRIMARY KEY (repository_id, sha, path),
    FOREIGN KEY (repository_id, sha) REFERENCES commits(repository_id, sha) ON DELETE CASCADE
) STRICT;

-- Lines introduced by one commit and first removed by another, aggregated per
-- pair. introduced_sha is 'baseline' for lines older than the replay boundary.
CREATE TABLE line_removals (
    repository_id INTEGER NOT NULL,
    introduced_sha TEXT NOT NULL,
    removed_sha TEXT NOT NULL,
    lines INTEGER NOT NULL,
    PRIMARY KEY (repository_id, introduced_sha, removed_sha),
    FOREIGN KEY (repository_id, removed_sha) REFERENCES commits(repository_id, sha) ON DELETE CASCADE
) STRICT;

CREATE INDEX line_removals_removed ON line_removals (repository_id, removed_sha);

CREATE TABLE pull_requests (
    repository_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    number INTEGER NOT NULL,
    node_id TEXT NOT NULL,
    author_login TEXT NOT NULL,
    title TEXT NOT NULL,
    url TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('open', 'closed', 'merged')),
    is_draft INTEGER NOT NULL CHECK (is_draft IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    merged_at TEXT,
    closed_at TEXT,
    PRIMARY KEY (repository_id, number)
) STRICT;

CREATE INDEX pull_requests_merged_at ON pull_requests (merged_at);
CREATE INDEX pull_requests_closed_at ON pull_requests (closed_at);
CREATE INDEX pull_requests_author ON pull_requests (author_login);

CREATE TABLE pr_commits (
    repository_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    sha TEXT NOT NULL,
    author_login TEXT,
    author_email TEXT NOT NULL,
    author_name TEXT NOT NULL,
    authored_at TEXT NOT NULL,
    committed_at TEXT NOT NULL,
    PRIMARY KEY (repository_id, number, sha),
    FOREIGN KEY (repository_id, number) REFERENCES pull_requests(repository_id, number) ON DELETE CASCADE
) STRICT;

CREATE INDEX pr_commits_author_login ON pr_commits (author_login);

-- Timeline items: ready_for_review, convert_to_draft, review_requested, merged, force_pushed.
CREATE TABLE pr_events (
    repository_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    actor_login TEXT,
    subject_login TEXT,
    at TEXT NOT NULL,
    PRIMARY KEY (repository_id, number, seq),
    FOREIGN KEY (repository_id, number) REFERENCES pull_requests(repository_id, number) ON DELETE CASCADE
) STRICT;

CREATE TABLE reviews (
    repository_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    node_id TEXT NOT NULL,
    author_login TEXT NOT NULL,
    state TEXT NOT NULL,
    submitted_at TEXT NOT NULL,
    PRIMARY KEY (repository_id, number, node_id),
    FOREIGN KEY (repository_id, number) REFERENCES pull_requests(repository_id, number) ON DELETE CASCADE
) STRICT;

CREATE INDEX reviews_author ON reviews (author_login, submitted_at);

CREATE TABLE comments (
    repository_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    node_id TEXT NOT NULL,
    author_login TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('issue', 'review')),
    created_at TEXT NOT NULL,
    PRIMARY KEY (repository_id, number, node_id),
    FOREIGN KEY (repository_id, number) REFERENCES pull_requests(repository_id, number) ON DELETE CASCADE
) STRICT;

CREATE INDEX comments_author ON comments (author_login, created_at);

-- Derived data: rebuilt for affected days after each sync, read by the dashboard.

-- One row per commit by a developer: local day and line counts by work type.
CREATE TABLE commit_stats (
    repository_id INTEGER NOT NULL,
    sha TEXT NOT NULL,
    developer_id TEXT NOT NULL,
    day TEXT NOT NULL,
    is_merge INTEGER NOT NULL CHECK (is_merge IN (0, 1)),
    lines_added INTEGER NOT NULL,
    lines_removed INTEGER NOT NULL,
    new_work INTEGER NOT NULL,
    legacy_refactor INTEGER NOT NULL,
    help_others INTEGER NOT NULL,
    churn INTEGER NOT NULL,
    PRIMARY KEY (repository_id, sha),
    FOREIGN KEY (repository_id, sha) REFERENCES commits(repository_id, sha) ON DELETE CASCADE
) STRICT;

CREATE INDEX commit_stats_developer_day ON commit_stats (developer_id, day);
CREATE INDEX commit_stats_day ON commit_stats (day);

CREATE TABLE pr_timings (
    repository_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    first_commit_at TEXT,
    ready_at TEXT,
    first_response_at TEXT,
    merged_at TEXT,
    coding_seconds INTEGER,
    pickup_seconds INTEGER,
    review_seconds INTEGER,
    cycle_seconds INTEGER,
    iterations INTEGER NOT NULL,
    is_unreviewed INTEGER NOT NULL CHECK (is_unreviewed IN (0, 1)),
    PRIMARY KEY (repository_id, number),
    FOREIGN KEY (repository_id, number) REFERENCES pull_requests(repository_id, number) ON DELETE CASCADE
) STRICT;
