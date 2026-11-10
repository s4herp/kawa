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
