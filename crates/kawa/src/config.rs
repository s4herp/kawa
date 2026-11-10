//! Loading and validation of `kawa.toml`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

pub const FILE_NAME: &str = "kawa.toml";

const WEIGHT_SUM_TOLERANCE: f64 = 0.001;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// SQLite file; `None` means the platform data directory.
    pub database: Option<PathBuf>,
    /// IANA timezone for day boundaries; `None` means the system timezone.
    pub timezone: Option<String>,
    /// Lines younger than this are churn or helping others when changed.
    pub recency_days: u32,
    /// Number of leading path segments that name a module.
    pub module_depth: usize,
    /// Minimum changed lines before efficiency and impact are shown.
    pub min_lines: u64,
    /// How far back the first sync reads pull requests and history.
    pub sync_window_days: u32,
    pub bot_pattern: String,
    /// Globs excluded in addition to the built-in lockfile, vendor and generated rules.
    pub exclude: Vec<String>,
    /// Commits treated like `.git-blame-ignore-revs` entries.
    pub ignore_revs: Vec<String>,
    pub impact: ImpactWeights,
    pub repositories: Vec<Repository>,
    pub developers: Vec<Developer>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            database: None,
            timezone: None,
            recency_days: 21,
            module_depth: 2,
            min_lines: 50,
            sync_window_days: 180,
            bot_pattern: r"(?i)\[bot\]$".to_owned(),
            exclude: Vec::new(),
            ignore_revs: Vec::new(),
            impact: ImpactWeights::default(),
            repositories: Vec::new(),
            developers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImpactWeights {
    pub lines: f64,
    pub files: f64,
    pub modules: f64,
}

impl Default for ImpactWeights {
    fn default() -> Self {
        Self {
            lines: 0.4,
            files: 0.3,
            modules: 0.3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    pub path: PathBuf,
    /// Branch to analyze; `None` means the remote default branch.
    pub branch: Option<String>,
    /// `owner/name` on GitHub; `None` disables pull request metrics.
    pub github: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Developer {
    /// Canonical email that identifies the developer.
    pub email: String,
    pub name: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub logins: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("invalid {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error(
        "invalid impact weights lines={lines}, files={files}, modules={modules}: \
         they must be non-negative and sum to 1.0"
    )]
    Weights {
        lines: f64,
        files: f64,
        modules: f64,
    },
    #[error("invalid bot_pattern {pattern:?}: {message}")]
    BotPattern { pattern: String, message: String },
    #[error("invalid exclude glob {glob:?}: {message}")]
    ExcludeGlob { glob: String, message: String },
    #[error("invalid github name {name:?} for {path}: expected owner/name")]
    GithubName { name: String, path: PathBuf },
    #[error("{field} must be greater than zero")]
    Zero { field: &'static str },
    #[error("unknown timezone {name:?}: use an IANA name such as America/Santiago")]
    Timezone { name: String },
}

impl Config {
    /// Loads the config from `path`, or from the platform config directory
    /// when `path` is `None`. A missing default file yields the defaults; a
    /// missing explicit file is an error.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        match path {
            Some(path) => Self::from_file(path),
            None => match default_path() {
                Some(path) if path.exists() => Self::from_file(&path),
                _ => Ok(Self::default()),
            },
        }
    }

    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&text).map_err(|error| match error {
            ConfigError::Parse { message, .. } => ConfigError::Parse {
                path: path.to_owned(),
                message,
            },
            other => other,
        })
    }

    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let mut config: Self = toml::from_str(text).map_err(|error| ConfigError::Parse {
            path: PathBuf::from(FILE_NAME),
            message: error.message().to_owned(),
        })?;
        config.validate()?;
        let home = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_owned());
        if let Some(home) = home {
            config.database = config.database.map(|p| expand_home(p, &home));
            for repository in &mut config.repositories {
                repository.path = expand_home(std::mem::take(&mut repository.path), &home);
            }
        }
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let ImpactWeights {
            lines,
            files,
            modules,
        } = self.impact;
        let any_negative = [lines, files, modules].iter().any(|w| *w < 0.0);
        if any_negative || ((lines + files + modules) - 1.0).abs() > WEIGHT_SUM_TOLERANCE {
            return Err(ConfigError::Weights {
                lines,
                files,
                modules,
            });
        }
        if self.recency_days == 0 {
            return Err(ConfigError::Zero {
                field: "recency_days",
            });
        }
        if self.module_depth == 0 {
            return Err(ConfigError::Zero {
                field: "module_depth",
            });
        }
        if self.sync_window_days == 0 {
            return Err(ConfigError::Zero {
                field: "sync_window_days",
            });
        }
        if let Some(name) = &self.timezone
            && jiff::tz::TimeZone::get(name).is_err()
        {
            return Err(ConfigError::Timezone { name: name.clone() });
        }
        regex::Regex::new(&self.bot_pattern).map_err(|error| ConfigError::BotPattern {
            pattern: self.bot_pattern.clone(),
            message: error.to_string(),
        })?;
        for glob in &self.exclude {
            globset::Glob::new(glob).map_err(|error| ConfigError::ExcludeGlob {
                glob: glob.clone(),
                message: error.to_string(),
            })?;
        }
        for repository in &self.repositories {
            if let Some(name) = &repository.github {
                let valid = matches!(
                    name.split_once('/'),
                    Some((owner, repo)) if !owner.is_empty() && !repo.is_empty() && !repo.contains('/')
                );
                if !valid {
                    return Err(ConfigError::GithubName {
                        name: name.clone(),
                        path: repository.path.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

fn expand_home(path: PathBuf, home: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => home.join(rest),
        Err(_) => path,
    }
}

impl Config {
    /// The configured timezone, or the system timezone.
    pub fn timezone(&self) -> jiff::tz::TimeZone {
        self.timezone
            .as_deref()
            .and_then(|name| jiff::tz::TimeZone::get(name).ok())
            .unwrap_or_else(jiff::tz::TimeZone::system)
    }

    pub fn impact_settings(&self) -> kawa_store::ImpactSettings {
        kawa_store::ImpactSettings {
            lines: self.impact.lines,
            files: self.impact.files,
            modules: self.impact.modules,
            module_depth: self.module_depth,
            min_lines: self.min_lines,
        }
    }
}

pub fn default_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "kawa").map(|dirs| dirs.config_dir().join(FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_explicit_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let result = Config::load(Some(&dir.path().join(FILE_NAME)));
        assert!(
            matches!(result, Err(ConfigError::Read { .. })),
            "should fail when an explicit config path does not exist"
        );
    }

    #[test]
    fn empty_file_yields_defaults() {
        assert_eq!(
            Config::parse("").unwrap(),
            Config::default(),
            "should use every default when the file sets nothing"
        );
    }

    #[test]
    fn defaults_match_the_design() {
        let config = Config::default();
        assert_eq!(config.recency_days, 21, "should default recency to 21 days");
        assert_eq!(config.module_depth, 2, "should default module depth to 2");
        assert_eq!(config.min_lines, 50, "should default minimum lines to 50");
        assert_eq!(
            config.sync_window_days, 180,
            "should default sync window to 180 days"
        );
        assert_eq!(
            config.impact,
            ImpactWeights {
                lines: 0.4,
                files: 0.3,
                modules: 0.3
            },
            "should default impact weights to 0.4, 0.3 and 0.3"
        );
        let bot = regex::Regex::new(&config.bot_pattern).unwrap();
        assert!(
            bot.is_match("depbot[bot]"),
            "should match names ending in [bot]"
        );
        assert!(!bot.is_match("Ana"), "should not match human names");
    }

    #[test]
    fn invalid_weights_are_rejected_by_name() {
        let error =
            Config::parse("[impact]\nlines = 0.6\nfiles = 0.3\nmodules = 0.3\n").unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("lines=0.6")
                && message.contains("files=0.3")
                && message.contains("modules=0.3"),
            "should name every weight when they do not sum to 1.0, got: {message}"
        );

        let error =
            Config::parse("[impact]\nlines = 1.2\nfiles = -0.2\nmodules = 0.0\n").unwrap_err();
        assert!(
            matches!(error, ConfigError::Weights { .. }),
            "should reject negative weights even when they sum to 1.0"
        );
    }

    #[test]
    fn weights_within_tolerance_are_accepted() {
        let config = Config::parse("[impact]\nlines = 0.3334\nfiles = 0.3333\nmodules = 0.3333\n");
        assert!(
            config.is_ok(),
            "should accept weights that sum to 1.0 within 0.001"
        );
    }

    #[test]
    fn unknown_key_is_reported() {
        let error = Config::parse("recency = 14\n").unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("unknown field") && message.contains("recency"),
            "should name the unknown key, got: {message}"
        );

        let error =
            Config::parse("[impact]\nlines = 0.4\nfiles = 0.3\nmodulez = 0.3\n").unwrap_err();
        assert!(
            error.to_string().contains("modulez"),
            "should name unknown keys inside nested tables"
        );
    }

    #[test]
    fn full_example_parses() {
        let text = r#"
            database = "/tmp/kawa.sqlite"
            timezone = "America/Santiago"
            recency_days = 14
            exclude = ["docs/generated/**"]
            ignore_revs = ["0123456789abcdef0123456789abcdef01234567"]

            [impact]
            lines = 0.5
            files = 0.25
            modules = 0.25

            [[repositories]]
            path = "/src/app"
            branch = "main"
            github = "acme/app"

            [[repositories]]
            path = "/src/tool"

            [[developers]]
            email = "ana@new.example"
            name = "Ana"
            aliases = ["ana@old.example"]
            logins = ["ana-gh"]
        "#;
        let config = Config::parse(text).unwrap();
        assert_eq!(
            config.repositories.len(),
            2,
            "should read both repositories"
        );
        assert_eq!(
            config.repositories[1].github, None,
            "should allow a repository without a GitHub name"
        );
        assert_eq!(
            config.developers[0].logins,
            vec!["ana-gh".to_owned()],
            "should read developer logins"
        );
        assert_eq!(
            config.recency_days, 14,
            "should override the recency default"
        );
        assert_eq!(
            config.min_lines, 50,
            "should keep defaults for fields not set"
        );
    }

    #[test]
    fn tilde_paths_expand_to_the_home_directory() {
        let home = Path::new("/home/ana");
        assert_eq!(
            expand_home(PathBuf::from("~/src/app"), home),
            PathBuf::from("/home/ana/src/app"),
            "should expand a leading ~ to the home directory"
        );
        assert_eq!(
            expand_home(PathBuf::from("/src/~app"), home),
            PathBuf::from("/src/~app"),
            "should leave paths without a leading ~ unchanged"
        );
    }

    #[test]
    fn invalid_github_name_is_rejected() {
        let error = Config::parse("[[repositories]]\npath = \"/src/app\"\ngithub = \"acme\"\n")
            .unwrap_err();
        assert!(
            matches!(error, ConfigError::GithubName { .. }),
            "should reject a GitHub name without owner/name"
        );
    }

    #[test]
    fn invalid_bot_pattern_and_glob_are_rejected() {
        assert!(
            matches!(
                Config::parse("bot_pattern = \"(\"\n"),
                Err(ConfigError::BotPattern { .. })
            ),
            "should reject a bot pattern that is not a valid regex"
        );
        assert!(
            matches!(
                Config::parse("exclude = [\"a/{b\"]\n"),
                Err(ConfigError::ExcludeGlob { .. })
            ),
            "should reject an invalid exclude glob"
        );
    }

    #[test]
    fn unknown_timezone_is_rejected() {
        assert!(
            matches!(
                Config::parse("timezone = \"Mars/Olympus\"\n"),
                Err(ConfigError::Timezone { .. })
            ),
            "should reject a timezone that is not an IANA name"
        );
    }

    #[test]
    fn zero_values_are_rejected() {
        assert!(
            matches!(
                Config::parse("module_depth = 0\n"),
                Err(ConfigError::Zero {
                    field: "module_depth"
                })
            ),
            "should reject a module depth of zero"
        );
    }

    #[test]
    fn file_errors_carry_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        fs::write(&path, "recency_days = \"x\"\n").unwrap();
        let message = Config::from_file(&path).unwrap_err().to_string();
        assert!(
            message.contains(&path.display().to_string()),
            "should include the file path in parse errors, got: {message}"
        );
    }
}
