//! Command-line options.  `missing_docs_in_private_items` is enabled because
//! Clap reads doc comments for CLI help text.

#![warn(clippy::missing_docs_in_private_items)]

use std::{io::IsTerminal, path::PathBuf};

use uuid::Uuid;

#[derive(clap::Parser)]
#[command(author, version, about)]
/// Top-level options for the hub command
pub struct Opts {
    /// Use a different Hub config file than the default
    ///
    /// By default, hub first searches the current directory for a file named
    /// .hub-config.toml, then falls back to the default location in the
    /// user configuration directory.  This location can be viewed by running
    /// `hub config path`.
    #[arg(short = 'C', long, global = true)]
    pub config: Option<PathBuf>,

    /// Name of the subcommand to run
    #[command(subcommand)]
    pub subcmd: Subcommand,
}

/// Top-level subcommands for hub
#[derive(clap::Subcommand)]
pub enum Subcommand {
    /// View and set configuration for the hub command
    Config(Config),
    /// Upload files to Hub
    Upload(Upload),
}

/// Options for hub config
#[derive(clap::Args)]
pub struct Config {
    /// Name of the subcommand to run
    #[command(subcommand)]
    pub subcmd: ConfigSubcommand,
}

/// Subcommands for hub config
#[derive(clap::Subcommand)]
pub enum ConfigSubcommand {
    /// Print the location of the config file currently used by other hub
    /// commands
    Path,
    /// Set the Hub GraphQL API endpoint
    GraphqlEndpoint(ConfigGraphqlEndpoint),
    /// Override or reset the Hub root endpoint
    HubEndpoint(ConfigHubEndpoint),
    /// Read a new Hub API token from STDIN
    Token,
}

/// Options for hub config graphql-endpoint
#[derive(clap::Args)]
pub struct ConfigGraphqlEndpoint {
    /// Specify the GraphQL API endpoint, required if not using a terminal,
    /// otherwise STDIN is used as the default
    #[arg(required = !std::io::stdin().is_terminal())]
    pub endpoint: Option<String>,
}

/// Options for hub config hub-endpoint
#[derive(clap::Args)]
pub struct ConfigHubEndpoint {
    /// Reset the endpoint override and infer it from the GraphQL API endpoint
    #[arg(short, long)]
    pub reset: bool,

    /// Override the root Hub endpoint, required if not using a terminal,
    /// otherwise STDIN is used as the default
    #[arg(required = !std::io::stdin().is_terminal(), conflicts_with("reset"))]
    pub endpoint: Option<String>,
}

/// Options for hub upload
#[derive(clap::Args)]
pub struct Upload {
    /// Name of the subcommand to run
    #[command(subcommand)]
    pub subcmd: UploadSubcommand,
}

/// Subcommands for hub upload
#[derive(clap::Subcommand)]
pub enum UploadSubcommand {
    /// Populate files for an open drop
    Drop(UploadDrop),
}

/// Options for hub upload drop
#[derive(clap::Args)]
pub struct UploadDrop {
    /// UUID of the drop to upload to
    #[arg(short = 'd', long = "drop")]
    pub drop_id: Uuid,

    /// Specify a search path for assets
    #[arg(short = 'I', long = "include")]
    pub include_dirs: Vec<PathBuf>,

    /// Limit the number of concurrently-running jobs
    #[arg(short = 'j', long = "jobs", default_value_t = 4)]
    pub jobs: u16,

    /// Path to a directory containing metadata JSON files to upload
    #[arg(required = true)]
    pub input_dirs: Vec<PathBuf>,

}
