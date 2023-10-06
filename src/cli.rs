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

/// Mixin options for concurrency
#[derive(clap::Args)]
pub struct ConcurrencyOpts {
    /// Limit the number of concurrently-running jobs
    #[arg(short = 'j', long = "jobs", default_value_t = 4)]
    pub jobs: u16,
}

/// Top-level subcommands for hub
#[derive(clap::Subcommand)]
pub enum Subcommand {
    /// View and set configuration for the hub command
    Config(Config),
    /// Mint queued NFTs from an open drop to a list of wallets
    Airdrop(Airdrop),
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
    /// Refresh the contents of the current config file, updating any
    /// deprecated properties
    Update,
    /// Set the Hub API endpoint
    ApiEndpoint(ConfigApiEndpoint),
    /// Read a new Hub API token from STDIN
    Token,
}

/// Options for hub config api-endpoint
#[derive(clap::Args)]
pub struct ConfigApiEndpoint {
    /// Print the current API endpoint
    #[arg(long)]
    pub get: bool,

    /// Specify the API endpoint, required if not using a terminal, otherwise
    /// STDIN is used as the default
    #[arg(required = !std::io::stdin().is_terminal(), conflicts_with("get"))]
    pub endpoint: Option<String>,
}

/// Options for hub airdrop
#[derive(clap::Args)]
pub struct Airdrop {
    /// Concurrency options
    #[command(flatten)]
    pub concurrency: ConcurrencyOpts,

    /// UUID of the open drop to mint from
    #[arg(short = 'd', long = "drop")]
    pub drop_id: Uuid,

    /// Do not mint compressed NFTs
    #[arg(long)]
    pub no_compressed: bool,

    /// Number of NFTs to mint to each wallet specified
    #[arg(short = 'n', long, default_value_t = 1)]
    pub mints_per_wallet: u32,

    /// Path to one or more files containing newline-separated wallet addresses
    /// to mint to
    ///
    /// Passing a single hyphen `-` will read from STDIN.
    #[arg(required = true)]
    pub wallets: Vec<PathBuf>,
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
    /// Concurrency options
    #[command(flatten)]
    pub concurrency: ConcurrencyOpts,

    /// UUID of the open drop to queue mints to
    #[arg(short = 'd', long = "drop")]
    pub drop_id: Uuid,

    /// Specify a search path for assets
    #[arg(short = 'I', long = "include")]
    pub include_dirs: Vec<PathBuf>,

    /// Path to a directory containing metadata JSON files to upload
    #[arg(required = true)]
    pub input_dirs: Vec<PathBuf>,
}
