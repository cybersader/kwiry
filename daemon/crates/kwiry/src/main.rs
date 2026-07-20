mod auth;
mod runtime;
mod server;
mod watcher;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use kwiry_core::{
    Paths, SearchRequest, add_vault, build_index, load_config, search_index, update_config,
};

#[derive(Debug, Parser)]
#[command(name = "kwiry", version, about = "Knowledge-work lexical search")]
struct Cli {
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[arg(long, global = true, value_name = "DIRECTORY")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage registered trees.
    Vault {
        #[command(subcommand)]
        command: VaultCommand,
    },
    /// Rebuild the disposable lexical index from registered files.
    Index,
    /// Run the long-lived HTTP search daemon and filesystem reconciler.
    Serve {
        #[arg(long, value_name = "ADDRESS")]
        bind: Option<String>,
        /// Load the local embedding model and serve semantic/hybrid modes.
        /// Downloads the model on first use; embeddings backfill at boot.
        #[arg(long)]
        semantic: bool,
    },
    /// Search the lexical index.
    Search {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, value_name = "VAULT_ID")]
        vault: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum VaultCommand {
    /// Register a Markdown/text tree.
    Add {
        #[arg(long)]
        id: String,
        #[arg(long, value_name = "DIRECTORY")]
        path: PathBuf,
        #[arg(long)]
        room: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::resolve(cli.config, cli.data_dir)?;

    match cli.command {
        Command::Vault {
            command: VaultCommand::Add { id, path, room },
        } => {
            let registration =
                update_config(&paths.config, |config| add_vault(config, id, path, room))?;
            println!(
                "Registered vault '{}' at {}",
                registration.id,
                registration.path.display()
            );
        }
        Command::Index => {
            let config = load_config(&paths.config).with_context(|| {
                format!(
                    "failed to load configuration from {}",
                    paths.config.display()
                )
            })?;
            let stats = build_index(&config, &paths.data_dir)?;
            for warning in &stats.warnings {
                eprintln!("warning: {}: {}", warning.path.display(), warning.message);
            }
            println!(
                "Indexed {} documents into {} chunks at {} ({} warnings)",
                stats.documents,
                stats.chunks,
                paths.data_dir.display(),
                stats.warnings.len()
            );
        }
        Command::Serve { bind, semantic } => server::serve(paths, bind, semantic).await?,
        Command::Search {
            query,
            limit,
            vault,
            json,
        } => {
            let hits = search_index(
                &paths.data_dir,
                &SearchRequest {
                    query,
                    limit,
                    vault_id: vault,
                },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&hits)?);
            } else if hits.is_empty() {
                println!("No results.");
            } else {
                for (index, hit) in hits.iter().enumerate() {
                    let heading = if hit.heading_path.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", hit.heading_path.join(" > "))
                    };
                    println!(
                        "{}. [{:.4}] {}:{}{}\n   {}",
                        index + 1,
                        hit.score,
                        hit.vault_id,
                        hit.path,
                        heading,
                        hit.excerpt.replace('\n', " ")
                    );
                }
            }
        }
    }

    Ok(())
}
