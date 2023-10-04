//! Command-line options.  `missing_docs_in_private_items` is enabled because
//! Clap reads doc comments for CLI help text.

#![warn(clippy::missing_docs_in_private_items)]

use std::{io::IsTerminal, path::PathBuf};

use uuid::Uuid;

/// Convert the parseable `ColorChoice` to `env_logger`'s `WriteStyle`
pub fn log_color(color: clap::ColorChoice) -> env_logger::WriteStyle {
    use clap::ColorChoice as C;
    use env_logger::WriteStyle as S;

    match color {
        C::Never => S::Never,
        C::Auto => S::Auto,
        C::Always => S::Always,
    }
}

#[derive(clap::Parser)]
#[command(author, version, about)]
/// Top-level options for the hub command
pub struct Opts {
    /// Adjust log level globally or on a per-module basis
    ///
    /// This flag uses the same syntax as the env_logger crate.
    #[arg(
        long,
        global = true,
        env = env_logger::DEFAULT_FILTER_ENV,
        default_value = "info",
    )]
    pub log_level: String,

    /// Adjust when to output colors to the terminal
    #[arg(
        long,
        global = true,
        env = env_logger::DEFAULT_WRITE_STYLE_ENV,
        default_value_t = clap::ColorChoice::Auto,
    )]
    pub color: clap::ColorChoice,

    /// Use a different Hub config file than the default
    ///
    /// By default, hub first searches the current directory for a file named
    /// .hub-config.toml, then falls back to the default location in the
    /// user configuration directory.  This location can be viewed by running
    /// `hub config path`.
    #[arg(short = 'C', long, global = true)]
    pub config: Option<PathBuf>,

    /// Cache options
    #[command(flatten)]
    pub cache: CacheOpts,

    /// Name of the subcommand to run
    #[command(subcommand)]
    pub subcmd: Subcommand,
}

/// Top-level command options related to the cache
#[derive(clap::Args)]
pub struct CacheOpts {
    /// Number of threads to use for cache management
    #[arg(long, global = true, default_value_t = num_cpus::get())]
    pub cache_threads: usize,

    /// Maximum cache write buffer size
    #[arg(long, global = true, default_value_t = 64 << 20)]
    pub cache_write_buf: usize,

    /// Maximum in-memory cache size
    #[arg(long, global = true, default_value_t = 128 << 20)]
    pub cache_lru_size: usize,
}

/// Top-level subcommands for hub
#[derive(clap::Subcommand)]
pub enum Subcommand {
    /// View and set configuration for the hub command
    Config(Config),
    /// Upload files to Hub
    Upload(Upload),
    Airdrop(Airdrop),
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
    /// Print the current GraphQL API endpoint
    #[arg(long)]
    pub get: bool,

    /// Specify the GraphQL API endpoint, required if not using a terminal,
    /// otherwise STDIN is used as the default
    #[arg(required = !std::io::stdin().is_terminal(), conflicts_with("get"))]
    pub endpoint: Option<String>,
}

/// Options for hub config hub-endpoint
#[derive(clap::Args)]
pub struct ConfigHubEndpoint {
    /// Print the current root Hub endpoint
    #[arg(long)]
    pub get: bool,

    /// Reset the endpoint override and infer it from the GraphQL API endpoint
    #[arg(short, long, conflicts_with("get"))]
    pub reset: bool,

    /// Override the root Hub endpoint, required if not using a terminal,
    /// otherwise STDIN is used as the default
    #[arg(
        required = !std::io::stdin().is_terminal(),
        conflicts_with("get"),
        conflicts_with("reset"),
    )]
    pub endpoint: Option<String>,
}

/// Options for hub upload
#[derive(clap::Args)]
pub struct Upload {
    /// Name of the subcommand to run
    #[command(subcommand)]
    pub subcmd: UploadSubcommand,
}

/// Options for hub upload
#[derive(clap::Args)]
pub struct Airdrop {
    #[arg(short = 'd', long = "drop")]
    pub drop_id: Uuid,

    #[arg(short = 'w')]
    pub wallets: PathBuf,

    #[arg(short = 'c', default_value_t = true)]
    pub compressed: bool,

    #[arg(short = 'n', default_value_t = 1)]
    pub mints_per_wallet: u32,

    #[arg(short = 'j', default_value_t = 8)]
    pub jobs: usize,
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
