use std::{
    fmt,
    io::{self, IsTerminal, Read},
};

use anyhow::{Context, Result};

use crate::{
    cli::{Config as Opts, ConfigGraphqlEndpoint, ConfigHubEndpoint, ConfigSubcommand},
    config::{Config, ConfigLocation},
};

fn mutate_config(
    config_location: &ConfigLocation,
    mutate: impl FnOnce(&mut Config) -> Result<()>,
) -> Result<()> {
    let config = config_location.load()?;
    let mut next_config = config.clone();
    mutate(&mut next_config)?;

    if next_config != config {
        next_config.save(config_location)?;
    }

    Ok(())
}

pub fn run(config: &ConfigLocation, opts: Opts) -> Result<()> {
    let Opts { subcmd } = opts;

    match subcmd {
        ConfigSubcommand::Path => {
            let canon = config.path().canonicalize().ok();
            println!("{}", canon.as_deref().unwrap_or(config.path()).display());
            Ok(())
        },
        ConfigSubcommand::GraphqlEndpoint(e) => mutate_config(config, |c| graphql_endpoint(c, e)),
        ConfigSubcommand::HubEndpoint(e) => mutate_config(config, |c| hub_endpoint(c, e)),
        ConfigSubcommand::Token => mutate_config(config, token),
    }
}

fn read_insecure<T: std::str::FromStr>(prompt: &str, bad_parse: impl fmt::Display) -> Result<T>
where T::Err: fmt::Display {
    let mut ed = rustyline::Editor::<(), _>::new().context("Error opening STDIN for reading")?;

    loop {
        let line = ed.readline(prompt).context("Error reading from STDIN")?;

        match line.parse() {
            Ok(u) => break Ok(u),
            Err(e) => println!("{bad_parse}: {e}"),
        }
    }
}

fn graphql_endpoint(config: &mut Config, endpoint: ConfigGraphqlEndpoint) -> Result<()> {
    let ConfigGraphqlEndpoint { endpoint } = endpoint;

    let endpoint = if let Some(e) = endpoint {
        e.parse().context("Invalid endpoint URL")?
    } else {
        read_insecure("Enter new GraphQL endpoint: ", "Invalid URL")?
    };

    config.set_graphql_endpoint(endpoint);

    Ok(())
}

fn hub_endpoint(config: &mut Config, endpoint: ConfigHubEndpoint) -> Result<()> {
    let ConfigHubEndpoint { reset, endpoint } = endpoint;

    let endpoint = if reset {
        None
    } else if let Some(e) = endpoint {
        Some(e.parse().context("Invalid endpoint URL")?)
    } else {
        Some(read_insecure("Enter new Hub endpoint: ", "Invalid URL")?)
    };

    config.set_hub_endpoint(endpoint);

    Ok(())
}

fn token(config: &mut Config) -> Result<()> {
    let token = if io::stdin().is_terminal() {
        rpassword::prompt_password("Enter new Hub API token: ")
            .context("Error reading from terminal")?
    } else {
        let mut s = String::new();

        io::stdin()
            .read_to_string(&mut s)
            .context("Error reading from STDIN")?;
        s
    };

    config.set_token(token);

    Ok(())
}
