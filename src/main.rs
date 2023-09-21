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
    pub mod config;
    pub mod upload_drop;
}

use anyhow::{Context, Result};
use cli::{Opts, Subcommand, UploadSubcommand};
use config::{Config, ConfigLocation};

fn main() {
    match run() {
        Ok(()) => (),
        Err(e) => {
            println!("ERROR: {e:?}");
            std::process::exit(1);
        },
    }
}

#[inline]
fn read(config: impl FnOnce(bool) -> Result<ConfigLocation>) -> Result<Config> {
    config(false)?.load()
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Error initializing async runtime")
}

fn run() -> Result<()> {
    let Opts { config, subcmd } = clap::Parser::parse();
    let config = |w| ConfigLocation::new(config, w);

    match subcmd {
        Subcommand::Config(c) => commands::config::run(&config(true)?, c),
        Subcommand::Upload(u) => match u.subcmd {
            UploadSubcommand::Drop(d) => commands::upload_drop::run(&read(config)?, d),
        },
    }
}
