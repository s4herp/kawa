use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use kawa::config::Config;
use kawa::github_sync::{self, GithubSync, PrOutcome};
use kawa::identities;
use kawa::sync::{self, RepositoryOutcome};
use kawa_store::Store;

#[derive(Parser)]
#[command(name = "kawa", version, about = "Local engineering flow analytics")]
struct Cli {
    /// Path to kawa.toml (default: the platform config directory)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read git history and GitHub pull requests into the local database
    Sync {
        /// Stop instead of waiting when the GitHub rate limit is exhausted
        #[arg(long)]
        no_wait: bool,
    },
    /// Serve the dashboard on localhost
    Serve {
        #[arg(long, default_value_t = 7878)]
        port: u16,
        /// Address to listen on
        #[arg(long, default_value = "127.0.0.1")]
        bind: std::net::IpAddr,
        /// Allow a non-loopback --bind address; the dashboard has no login
        #[arg(long)]
        allow_remote: bool,
    },
    /// List every raw identity and the developer it resolves to
    Identities {
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Table,
    Json,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("kawa: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let config = Config::load(cli.config.as_deref()).map_err(|e| e.to_string())?;
    let mut store = open_store(&config)?;
    match cli.command {
        Command::Sync { no_wait } => {
            let now = jiff::Timestamp::now();
            let outcomes = sync::sync_git(&mut store, &config, now).map_err(|e| e.to_string())?;
            let mut github = GithubSync {
                token: github_sync::discover_token(),
                api_url: kawa_github::DEFAULT_API_URL.to_owned(),
                wait: github_sync::wait_policy(no_wait),
            };
            let pr_outcomes =
                github_sync::sync_pull_requests(&mut store, &config, now, &mut github)
                    .map_err(|e| e.to_string())?;
            sync::refresh(&mut store, &config).map_err(|e| e.to_string())?;
            let mut failed = 0;
            for outcome in &pr_outcomes {
                match outcome {
                    PrOutcome::Synced {
                        path,
                        pull_requests,
                    } => {
                        println!("synced {path}: {pull_requests} pull requests updated");
                    }
                    PrOutcome::NotConfigured { .. } => {}
                    PrOutcome::Skipped { path, reason } => eprintln!("skipped {path}: {reason}"),
                    PrOutcome::Failed { path, error } => {
                        failed += 1;
                        eprintln!("failed {path} pull requests: {error}");
                    }
                }
            }
            for outcome in &outcomes {
                match outcome {
                    RepositoryOutcome::Synced {
                        path,
                        head,
                        commits,
                    } => {
                        println!("synced {path} at {}: {commits} commits", &head[..12]);
                    }
                    RepositoryOutcome::Failed { path, error } => {
                        failed += 1;
                        eprintln!("failed {path}: {error}");
                    }
                }
            }
            if failed > 0 {
                return Err(format!(
                    "{failed} of {} repositories failed",
                    outcomes.len()
                ));
            }
        }
        Command::Serve {
            port,
            bind,
            allow_remote,
        } => {
            kawa::web::check_bind(bind, allow_remote)?;
            let state = std::sync::Arc::new(kawa::web::AppState {
                store: std::sync::Mutex::new(store),
                config,
                clock: std::sync::Arc::new(jiff::Timestamp::now),
            });
            let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            runtime
                .block_on(kawa::web::serve(
                    state,
                    std::net::SocketAddr::new(bind, port),
                ))
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        Command::Identities { format } => {
            // Derived tables are keyed by developer, so they follow the identities.
            sync::refresh(&mut store, &config).map_err(|e| e.to_string())?;
            let entries = identities::audit(&store).map_err(|e| e.to_string())?;
            let output = match format {
                Format::Json => identities::render_json(entries),
                Format::Table => identities::render_table(&entries),
            };
            print!("{output}");
        }
    }
    Ok(())
}

fn open_store(config: &Config) -> Result<Store, String> {
    let path = match &config.database {
        Some(path) => path.clone(),
        None => directories::ProjectDirs::from("", "", "kawa")
            .map(|dirs| dirs.data_dir().join("kawa.sqlite"))
            .ok_or("cannot determine the data directory; set `database` in kawa.toml")?,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    Store::open(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))
}
