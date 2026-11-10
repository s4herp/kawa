//! Resolving stored identities to developers and rendering the identity audit.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use kawa_core::identity::{
    DeveloperId, DeveloperSpec, DisplayNames, IdentityError, IdentityRules, LoginEvidence,
    Resolution, link_logins,
};
use kawa_core::repo;
use kawa_store::{AuditRow, IdentityResolution, Store, StoreError};
use serde::Serialize;
use thiserror::Error;

use crate::config::Config;

#[derive(Debug, Error)]
pub enum IdentitiesError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Rules(#[from] IdentityError),
}

pub fn developer_specs(config: &Config) -> Vec<DeveloperSpec> {
    config
        .developers
        .iter()
        .map(|d| DeveloperSpec {
            email: d.email.clone(),
            name: d.name.clone(),
            aliases: d.aliases.clone(),
            logins: d.logins.clone(),
        })
        .collect()
}

/// Builds the rules from config and the `.mailmap` committed on each
/// repository's analyzed branch. Repositories that cannot be read are skipped;
/// sync reports them.
pub fn rules_from_config(config: &Config) -> Result<IdentityRules, IdentitiesError> {
    let mut mailmaps = Vec::new();
    for repository in &config.repositories {
        let Ok(opened) = repo::open(&repository.path) else {
            continue;
        };
        let Ok(tip) = repo::branch_tip(&opened, repository.branch.as_deref()) else {
            continue;
        };
        if let Ok(Some(mailmap)) = repo::mailmap_at(&tip) {
            mailmaps.push(mailmap);
        }
    }
    let buffers: Vec<&[u8]> = mailmaps.iter().map(Vec::as_slice).collect();
    Ok(IdentityRules::new(
        &buffers,
        &developer_specs(config),
        &config.bot_pattern,
    )?)
}

/// Re-resolves every stored identity, so config and mailmap edits apply to
/// all history.
pub fn resolve(
    store: &mut Store,
    rules: &IdentityRules,
    developers: &[DeveloperSpec],
) -> Result<(), IdentitiesError> {
    store.register_pr_logins()?;
    let evidence = store.login_evidence()?;
    let links = link_logins(
        rules,
        evidence.iter().map(|e| LoginEvidence {
            login: &e.login,
            name: &e.name,
            email: &e.email,
            commits: e.commits,
        }),
    );

    let mut names = DisplayNames::new(developers);
    let mut resolutions = Vec::new();
    let mut ids: BTreeSet<DeveloperId> = BTreeSet::new();
    for identity in store.identities()? {
        let resolution = if identity.login.is_empty() {
            rules.resolve_author(&identity.name, &identity.email)
        } else {
            rules.resolve_login(&identity.login, links.get(&identity.login.to_lowercase()))
        };
        match resolution {
            Resolution::Developer { id, name } => {
                if identity.login.is_empty() {
                    names.observe(&id, &name, identity.commits);
                }
                resolutions.push(IdentityResolution {
                    identity_id: identity.id,
                    developer_id: Some(id.as_str().to_owned()),
                    excluded: false,
                });
                ids.insert(id);
            }
            Resolution::Bot => resolutions.push(IdentityResolution {
                identity_id: identity.id,
                developer_id: None,
                excluded: true,
            }),
        }
    }
    let developers: Vec<(String, String)> = ids
        .iter()
        .map(|id| (id.as_str().to_owned(), names.name(id)))
        .collect();
    store.save_resolutions(&resolutions, &developers)?;
    Ok(())
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AuditEntry {
    pub identity: String,
    pub status: &'static str,
    pub developer_id: Option<String>,
    pub developer: Option<String>,
    pub commits: u64,
    pub pull_requests: u64,
    pub reviews: u64,
}

impl From<AuditRow> for AuditEntry {
    fn from(row: AuditRow) -> Self {
        let identity = if row.login.is_empty() {
            format!("{} <{}>", row.name, row.email)
        } else {
            format!("@{}", row.login)
        };
        let status = match (&row.developer_id, row.excluded) {
            (_, true) => "excluded: bot",
            (Some(id), false) if id.starts_with("gh:") => "unlinked",
            (Some(_), false) => "developer",
            (None, false) => "unresolved",
        };
        Self {
            identity,
            status,
            developer_id: row.developer_id,
            developer: row.developer_name,
            commits: row.commits,
            pull_requests: row.pull_requests,
            reviews: row.reviews,
        }
    }
}

#[derive(Debug, Serialize)]
struct AuditDocument {
    identities: Vec<AuditEntry>,
}

pub fn audit(store: &Store) -> Result<Vec<AuditEntry>, IdentitiesError> {
    Ok(store
        .identity_audit()?
        .into_iter()
        .map(AuditEntry::from)
        .collect())
}

pub fn render_json(entries: Vec<AuditEntry>) -> String {
    let mut text = serde_json::to_string_pretty(&AuditDocument {
        identities: entries,
    })
    .expect("audit serializes");
    text.push('\n');
    text
}

pub fn render_table(entries: &[AuditEntry]) -> String {
    let header = [
        "IDENTITY",
        "STATUS",
        "DEVELOPER",
        "COMMITS",
        "PRS",
        "REVIEWS",
    ];
    let rows: Vec<[String; 6]> = entries
        .iter()
        .map(|e| {
            [
                e.identity.clone(),
                e.status.to_owned(),
                e.developer.clone().unwrap_or_default(),
                e.commits.to_string(),
                e.pull_requests.to_string(),
                e.reviews.to_string(),
            ]
        })
        .collect();
    let mut widths = header.map(str::len);
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    let mut line = |cells: &[&str]| {
        let mut text = String::new();
        for (index, (cell, width)) in cells.iter().zip(widths).enumerate() {
            let pad = width - cell.chars().count();
            if index >= 3 {
                let _ = write!(text, "{}{}  ", " ".repeat(pad), cell);
            } else {
                let _ = write!(text, "{}{}  ", cell, " ".repeat(pad));
            }
        }
        out.push_str(text.trim_end());
        out.push('\n');
    };
    line(&header);
    for row in &rows {
        let cells: Vec<&str> = row.iter().map(String::as_str).collect();
        line(&cells);
    }
    out
}
