//! Entry point for the hub command

#![deny(
    clippy::disallowed_methods,
    clippy::suspicious,
    clippy::style,
    clippy::clone_on_ref_ptr,
    missing_debug_implementations,
    missing_copy_implementations
)]
#![warn(clippy::pedantic, missing_docs)]
#![allow(clippy::module_name_repetitions)]

mod cache;
mod cli;
mod config;

mod commands {
    pub mod airdrop;
    pub mod config;
    pub mod upload_drop;
}

mod common {
    pub mod concurrent;
    pub mod graphql;
    pub mod metadata_json;
    pub mod pubkey;
    pub mod reqwest;
    pub mod stats;
    pub mod tokio;
    pub mod toposort;
    pub mod url_permissive;
}

fn main() {
    match entry::run() {
        Ok(()) => (),
        Err(e) => {
            println!("ERROR: {e:?}");
            std::process::exit(1);
        },
    }
}

mod entry {
    use anyhow::Result;

    use crate::{
        cache::CacheConfig,
        cli::{log_color, Opts, Subcommand, UploadSubcommand},
        commands::{airdrop, config, upload_drop},
        config::{Config, ConfigLocation},
    };

    #[inline]
    fn read(config: impl FnOnce(bool) -> Result<ConfigLocation>) -> Result<Config> {
        config(false)?.load()
    }

    pub fn run() -> Result<()> {
        let Opts {
            log_level,
            color,
            config,
            cache,
            subcmd,
        } = clap::Parser::parse();

        env_logger::builder()
            .parse_filters(&log_level)
            .write_style(log_color(color))
            .init();

        let config = |w| ConfigLocation::new(config, w);
        let cache = CacheConfig::new(cache);

        match subcmd {
            Subcommand::Config(c) => config::run(&config(true)?, c),
            Subcommand::Airdrop(a) => airdrop::run(&read(config)?, cache, a),
            Subcommand::Upload(u) => match u.subcmd {
                UploadSubcommand::Drop(d) => upload_drop::run(&read(config)?, cache, d),
            },
        }
    }
}
