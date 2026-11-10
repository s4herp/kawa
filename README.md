# kawa

Local engineering flow analytics for git repositories and their GitHub pull requests: code fundamentals, a per-day work log, and review and collaboration metrics. Everything runs on your machine; data lives in one SQLite file.

kawa shows activity patterns and the commits and pull requests behind every number. It does not rank people and is not a performance evaluation.

## Configuration

kawa reads `kawa.toml` from the platform config directory (`~/Library/Application Support/kawa/kawa.toml` on macOS, `~/.config/kawa/kawa.toml` on Linux) or from the path given with `--config`. A missing default file means every default applies; a missing file passed with `--config` is an error. Unknown keys are rejected so typos do not pass silently.

```toml
# SQLite file. Default: the platform data directory.
database = "~/kawa/kawa.sqlite"

# IANA timezone for day boundaries. Default: the system timezone.
timezone = "America/Santiago"

# Lines younger than this many days count as churn (own) or helping others
# (someone else's) when changed; older lines count as legacy refactor.
recency_days = 21

# Leading path segments that name a module for impact: with 2,
# apps/billing/lib/a.ex belongs to module apps/billing.
module_depth = 2

# Developers with fewer changed lines in a period show "insufficient data"
# for efficiency and impact.
min_lines = 50

# How far back the first sync reads history and pull requests.
sync_window_days = 180

# Identities whose name or GitHub login matches this regex are bots and are
# excluded from every metric.
bot_pattern = '(?i)\[bot\]$'

# Extra paths to ignore, on top of lockfiles, vendored and generated code.
exclude = ["docs/generated/**"]

# Commits handled like .git-blame-ignore-revs entries (bulk formatting, moves).
ignore_revs = ["0123456789abcdef0123456789abcdef01234567"]

# Impact weights for changed lines, distinct files and distinct modules.
# They must be non-negative and sum to 1.0.
[impact]
lines = 0.4
files = 0.3
modules = 0.3

[[repositories]]
path = "~/src/app"
branch = "main"          # default: the remote default branch
github = "acme/app"      # omit for git-only metrics

[[developers]]
email = "ana@new.example"            # canonical email, the developer's id
name = "Ana Pérez"                   # display name; default: most frequent name seen
aliases = ["ana@personal.example"]   # extra emails for this developer
logins = ["ana-gh"]                  # GitHub logins for this developer
```

## Identities

Every commit author (`name <email>`) and every GitHub login is resolved to one developer, in this order:

1. The `.mailmap` committed on each repository's analyzed branch, with git's matching rules. Uncommitted edits to `.mailmap` are ignored.
2. `aliases` from `[[developers]]`, matched on email without regard to case.
3. Emails that differ only in case are the same developer. The display name is the configured `name`, or else the name used on most commits.
4. A GitHub login is linked to the developer that most of its pull request commits resolve to. `logins` in config override that link. A login with no link (for example someone who only reviews) becomes its own developer, shown as `unlinked`.
5. Names and logins matching `bot_pattern` are bots: their commits, pull requests, reviews and comments are excluded.

Resolution runs again on every sync, so config and mailmap changes apply to all history.

To check the result:

```sh
kawa identities              # table
kawa identities --format json
```

Each row is a raw identity with its status (`developer`, `unlinked`, `excluded: bot`), the developer it resolves to, and its commits, pull requests and reviews.

## Code fundamentals

Fundamentals cover non-ignored commits by developers (bots excluded) whose author date falls in the selected period. Days start and end at midnight in the configured `timezone`.

- **Active days**: distinct days with at least one non-merge commit, across every selected repository.
- **Commits per active day**: non-merge commits divided by active days.
- **Work types**: every changed line gets exactly one type.
  - Added lines beyond the lines removed in the same hunk are **new work**. A hunk that replaces 10 lines with 30 has 10 changed lines classified by the removed lines, plus 20 lines of new work.
  - A removed or replaced line is **churn** when its own author changes it less than `recency_days` (default 21) after writing it, **helping others** when someone else does, and **legacy refactor** when it is older, predates the analyzed history, or was written by a bot.
- **Efficiency**: the share of changed lines that are not churn. With 100 churn lines out of 1,000 changed lines, efficiency is 90%.
- **Impact**: `lines × L + files × F + modules × M` with the weights from `[impact]`, where L, F and M are the developer's percentile ranks among the selected developers for lines changed (added plus removed), distinct files and distinct modules. A module is the file's directory cut to `module_depth` segments; root files form module `.`. Impact is relative to the selection: it changes when the selected developers change, and values from different selections are not comparable.
- **Insufficient data**: below `min_lines` changed lines (default 50), efficiency and impact are not shown.

Work types are computed from stored history whenever kawa syncs, so changing `recency_days`, `timezone`, aliases or the bot pattern applies to all history on the next `kawa sync`.

Every value can be traced to its commits: each commit lists its lines per work type and the earlier commits whose lines it rewrote.

## Review and collaboration

Review metrics cover pull requests merged or closed in the selected period (by local day). PRs opened by bots are left out, and comments or reviews by bots never count as a response.

- **Ready for review**: when the PR was opened, or, for a PR opened as a draft, when it was first marked ready. Time spent as a draft before that is not measured.
- **Cycle time**: first commit to merge, split into
  - **coding**: first commit to ready for review;
  - **pickup** (also the time to first comment): ready for review to the first review or comment by someone other than the author;
  - **review**: that first response to merge.
- **Unreviewed**: merged with no review or comment from anyone but the author. Reported per author as a count and a share of merged PRs.
- **Reviewer reaction time**: from when a reviewer was requested (or from ready for review, when never requested) to their first review or comment on that PR.
- **Author responsiveness**: from a review or comment by someone else to the author's next commit on the PR or reply. GitHub does not report push times reliably, so the commit's committer date stands in for it.
- **Iterations**: review rounds by others that were followed by at least one new commit from the author before merge.
- **Who reviews whom**: for each reviewer and author, the PRs the reviewer submitted a review on.

Timings are reported as median and 75th percentile (nearest rank) in calendar hours, with the number of PRs they come from. Each PR shows the events its timings were computed from: commits, ready and draft changes, review requests, reviews, comments and the merge, with times and actors.
