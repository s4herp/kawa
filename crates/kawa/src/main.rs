use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use kawa::config::Config;
use kawa::identities;
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
        Command::Identities { format } => {
            let rules = identities::rules_from_config(&config).map_err(|e| e.to_string())?;
            identities::resolve(&mut store, &rules, &identities::developer_specs(&config))
                .map_err(|e| e.to_string())?;
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
