//! Resolution of raw git authors and GitHub logins into canonical developers.
//!
//! Order: mailmap, config aliases, email case-folding, login links, bot pattern.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use gix::mailmap::Snapshot;
use regex::Regex;
use thiserror::Error;

/// Stable developer key: the lowercased canonical email, or `gh:<login>` for a
/// GitHub login with no linked email.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeveloperId(String);

impl DeveloperId {
    pub fn from_email(email: &str) -> Self {
        Self(email.trim().to_lowercase())
    }

    pub fn from_login(login: &str) -> Self {
        Self(format!("gh:{}", login.trim().to_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_unlinked_login(&self) -> bool {
        self.0.starts_with("gh:")
    }
}

impl fmt::Display for DeveloperId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A developer declared in config.
#[derive(Debug, Clone, Default)]
pub struct DeveloperSpec {
    pub email: String,
    pub name: Option<String>,
    pub aliases: Vec<String>,
    pub logins: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// A person; `name` is the name seen on this identity after mailmap.
    Developer {
        id: DeveloperId,
        name: String,
    },
    Bot,
}

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("invalid bot pattern {pattern:?}: {message}")]
    BotPattern { pattern: String, message: String },
}

pub struct IdentityRules {
    mailmap: Snapshot,
    aliases: HashMap<String, DeveloperId>,
    logins: HashMap<String, DeveloperId>,
    bot: Regex,
}

impl IdentityRules {
    /// `mailmaps` are the raw `.mailmap` contents of every analyzed repository;
    /// entries are merged.
    pub fn new(
        mailmaps: &[&[u8]],
        developers: &[DeveloperSpec],
        bot_pattern: &str,
    ) -> Result<Self, IdentityError> {
        let mut mailmap = Snapshot::default();
        for buf in mailmaps {
            mailmap.merge(gix::mailmap::parse_ignore_errors(buf));
        }
        let mut aliases = HashMap::new();
        let mut logins = HashMap::new();
        for developer in developers {
            let id = DeveloperId::from_email(&developer.email);
            aliases.insert(id.as_str().to_owned(), id.clone());
            for alias in &developer.aliases {
                aliases.insert(alias.trim().to_lowercase(), id.clone());
            }
            for login in &developer.logins {
                logins.insert(login.trim().to_lowercase(), id.clone());
            }
        }
        let bot = Regex::new(bot_pattern).map_err(|error| IdentityError::BotPattern {
            pattern: bot_pattern.to_owned(),
            message: error.to_string(),
        })?;
        Ok(Self {
            mailmap,
            aliases,
            logins,
            bot,
        })
    }

    pub fn resolve_author(&self, name: &str, email: &str) -> Resolution {
        if self.bot.is_match(name.trim()) {
            return Resolution::Bot;
        }
        let signature = gix::actor::SignatureRef {
            name: name.into(),
            email: email.into(),
            time: "",
        };
        let (name, email) = match self.mailmap.try_resolve(signature) {
            Some(mapped) => (mapped.name.to_string(), mapped.email.to_string()),
            None => (name.to_owned(), email.to_owned()),
        };
        let name = name.trim().to_owned();
        if self.bot.is_match(&name) {
            return Resolution::Bot;
        }
        let folded = DeveloperId::from_email(&email);
        let id = self.aliases.get(folded.as_str()).cloned().unwrap_or(folded);
        Resolution::Developer { id, name }
    }

    /// Resolves a GitHub login. Config links win over `linked`, which comes
    /// from the emails GitHub reports on the login's PR commits.
    pub fn resolve_login(&self, login: &str, linked: Option<&DeveloperId>) -> Resolution {
        let login = login.trim();
        if self.bot.is_match(login) {
            return Resolution::Bot;
        }
        let id = self
            .logins
            .get(&login.to_lowercase())
            .or(linked)
            .cloned()
            .unwrap_or_else(|| DeveloperId::from_login(login));
        Resolution::Developer {
            id,
            name: login.to_owned(),
        }
    }
}

/// One PR commit as reported by GitHub: the login that authored it and the
/// git identity on the commit.
#[derive(Debug, Clone)]
pub struct LoginEvidence<'a> {
    pub login: &'a str,
    pub name: &'a str,
    pub email: &'a str,
    pub commits: u64,
}

/// Links each login to the developer that most of its PR commits resolve to.
/// Ties go to the smallest developer id so results are deterministic.
pub fn link_logins<'a>(
    rules: &IdentityRules,
    evidence: impl IntoIterator<Item = LoginEvidence<'a>>,
) -> BTreeMap<String, DeveloperId> {
    let mut votes: BTreeMap<String, BTreeMap<DeveloperId, u64>> = BTreeMap::new();
    for item in evidence {
        if let Resolution::Developer { id, .. } = rules.resolve_author(item.name, item.email) {
            *votes
                .entry(item.login.trim().to_lowercase())
                .or_default()
                .entry(id)
                .or_default() += item.commits;
        }
    }
    votes
        .into_iter()
        .filter_map(|(login, counts)| {
            counts
                .into_iter()
                .max_by(|(a_id, a), (b_id, b)| a.cmp(b).then_with(|| b_id.cmp(a_id)))
                .map(|(id, _)| (login, id))
        })
        .collect()
}

/// Picks a display name per developer: the config name if given, otherwise
/// the most frequent name seen, ties broken by the name that sorts first.
#[derive(Debug, Default)]
pub struct DisplayNames {
    fixed: HashMap<DeveloperId, String>,
    seen: BTreeMap<DeveloperId, BTreeMap<String, u64>>,
}

impl DisplayNames {
    pub fn new(developers: &[DeveloperSpec]) -> Self {
        let fixed = developers
            .iter()
            .filter_map(|d| {
                d.name
                    .as_ref()
                    .map(|name| (DeveloperId::from_email(&d.email), name.clone()))
            })
            .collect();
        Self {
            fixed,
            seen: BTreeMap::new(),
        }
    }

    pub fn observe(&mut self, id: &DeveloperId, name: &str, count: u64) {
        *self
            .seen
            .entry(id.clone())
            .or_default()
            .entry(name.to_owned())
            .or_default() += count;
    }

    pub fn name(&self, id: &DeveloperId) -> String {
        if let Some(name) = self.fixed.get(id) {
            return name.clone();
        }
        self.seen
            .get(id)
            .and_then(|names| {
                names
                    .iter()
                    .max_by(|(a_name, a), (b_name, b)| a.cmp(b).then_with(|| b_name.cmp(a_name)))
                    .map(|(name, _)| name.clone())
            })
            .unwrap_or_else(|| id.as_str().trim_start_matches("gh:").to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOT: &str = r"(?i)\[bot\]$";

    fn dev(id: &str, name: &str) -> Resolution {
        Resolution::Developer {
            id: DeveloperId::from_email(id),
            name: name.to_owned(),
        }
    }

    #[test]
    fn config_email_is_its_own_alias_after_case_folding() {
        let rules = IdentityRules::new(
            &[],
            &[DeveloperSpec {
                email: "Ana@New.Example".into(),
                ..Default::default()
            }],
            BOT,
        )
        .unwrap();
        assert_eq!(
            rules.resolve_author("Ana", "ANA@new.example"),
            dev("ana@new.example", "Ana"),
            "should fold the configured email and the commit email to one id"
        );
    }

    #[test]
    fn bot_matches_raw_or_mapped_name() {
        let mailmap = b"refresher[bot] <refresher@example.com> Refresher <refresher@example.com>\n";
        let rules = IdentityRules::new(&[mailmap], &[], BOT).unwrap();
        assert_eq!(
            rules.resolve_author(
                "depbot[bot]",
                "12345678+depbot[bot]@users.noreply.github.com"
            ),
            Resolution::Bot,
            "should exclude names ending in [bot]"
        );
        assert_eq!(
            rules.resolve_author("Refresher", "refresher@example.com"),
            Resolution::Bot,
            "should exclude identities the mailmap maps to a bot name"
        );
    }

    #[test]
    fn login_resolution_prefers_config_then_link_then_unlinked() {
        let rules = IdentityRules::new(
            &[],
            &[DeveloperSpec {
                email: "ana@new.example".into(),
                logins: vec!["Ana-GH".into()],
                ..Default::default()
            }],
            BOT,
        )
        .unwrap();
        let linked = DeveloperId::from_email("other@example.com");
        assert_eq!(
            rules.resolve_login("ana-gh", Some(&linked)),
            Resolution::Developer {
                id: DeveloperId::from_email("ana@new.example"),
                name: "ana-gh".into()
            },
            "should let a config login link override the evidence link"
        );
        assert_eq!(
            rules.resolve_login("luis", Some(&linked)),
            Resolution::Developer {
                id: linked.clone(),
                name: "luis".into()
            },
            "should use the evidence link when config has none"
        );
        assert_eq!(
            rules.resolve_login("Reviewer", None),
            Resolution::Developer {
                id: DeveloperId::from_login("reviewer"),
                name: "Reviewer".into()
            },
            "should make an unlinked login its own developer"
        );
        assert!(
            DeveloperId::from_login("reviewer").is_unlinked_login(),
            "should mark login-only developers as unlinked"
        );
        assert_eq!(
            rules.resolve_login("build-runner[bot]", None),
            Resolution::Bot,
            "should exclude bot logins"
        );
    }

    #[test]
    fn links_login_to_the_majority_developer() {
        let mailmap = b"Ana <ana@new.example> <ana@old.example>\n";
        let rules = IdentityRules::new(&[mailmap], &[], BOT).unwrap();
        let links = link_logins(
            &rules,
            [
                LoginEvidence {
                    login: "ana-gh",
                    name: "Ana",
                    email: "ana@old.example",
                    commits: 3,
                },
                LoginEvidence {
                    login: "ana-gh",
                    name: "Ana",
                    email: "ana@new.example",
                    commits: 2,
                },
                LoginEvidence {
                    login: "ana-gh",
                    name: "Pair",
                    email: "pair@example.com",
                    commits: 4,
                },
                LoginEvidence {
                    login: "bot",
                    name: "x[bot]",
                    email: "x@example.com",
                    commits: 9,
                },
            ],
        );
        assert_eq!(
            links.get("ana-gh"),
            Some(&DeveloperId::from_email("ana@new.example")),
            "should count both mailmapped emails as one developer and pick the majority"
        );
        assert!(
            !links.contains_key("bot"),
            "should not link logins whose commits are all bots"
        );
    }

    #[test]
    fn display_name_is_config_or_most_frequent() {
        let ana = DeveloperId::from_email("ana@x.example");
        let luis = DeveloperId::from_email("luis@x.example");
        let mut names = DisplayNames::new(&[DeveloperSpec {
            email: "luis@x.example".into(),
            name: Some("Luis Vidal".into()),
            ..Default::default()
        }]);
        names.observe(&ana, "ana perez", 2);
        names.observe(&ana, "Ana Pérez", 5);
        names.observe(&luis, "lvidal", 10);
        assert_eq!(
            names.name(&ana),
            "Ana Pérez",
            "should use the most frequent name"
        );
        assert_eq!(
            names.name(&luis),
            "Luis Vidal",
            "should prefer the configured name"
        );
        assert_eq!(
            names.name(&DeveloperId::from_login("reviewer")),
            "reviewer",
            "should fall back to the login for unseen login developers"
        );
    }
}
