use std::{
    borrow::Cow,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use directories::ProjectDirs;
use itertools::Itertools;
use log::warn;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use url::Url;

pub const LOCAL_NAME: &str = ".hub-config.toml";

#[inline]
pub fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("com", "Holaplex", "Hub")
        .context("Error finding default Hub config directory")
}

#[inline]
pub fn config_path(dirs: &ProjectDirs) -> PathBuf { dirs.config_dir().join("config.toml") }

fn swap_path(mut path: PathBuf) -> Option<PathBuf> {
    let mut name = path.file_name()?.to_string_lossy().into_owned();
    name.push('~');
    path.set_file_name(name);
    Some(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ConfigMode {
    ExplicitWrite,
    ExplicitRead,
    Implicit,
}

pub struct ConfigLocation {
    path: PathBuf,
    mode: ConfigMode,
}

impl ConfigLocation {
    pub fn new(path: Option<PathBuf>, writable: bool) -> Result<Self> {
        let mode = match (&path, writable) {
            (Some(_), true) => ConfigMode::ExplicitWrite,
            (Some(_), false) => ConfigMode::ExplicitRead,
            (None, _) => ConfigMode::Implicit,
        };
        let path = path.map_or_else(
            || {
                let local = PathBuf::from(LOCAL_NAME);
                // TODO: this should probably be expanded to search upward recursively
                let local_exists = local.try_exists().unwrap_or_else(|e| {
                    warn!("Error checking for local Hub config: {e}");
                    false
                });

                Result::<_>::Ok(if local_exists {
                    local
                } else {
                    config_path(&project_dirs()?)
                })
            },
            Ok,
        )?;

        Ok(Self { path, mode })
    }

    #[inline]
    pub fn load(&self) -> Result<Config> { Config::load(self) }

    #[inline]
    pub fn path(&self) -> &Path { &self.path }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ConfigFile<'a> {
    hub: Cow<'a, Config>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    graphql_endpoint: Url,
    hub_endpoint: Option<Url>,
    token: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            graphql_endpoint: "https://api.holaplex.com/graphql".try_into().unwrap(),
            hub_endpoint: None,
            token: String::default(),
        }
    }
}

impl Config {
    pub fn load(location: &ConfigLocation) -> Result<Self> {
        let text = match fs::read_to_string(&location.path) {
            Ok(s) => s,
            Err(e)
                if e.kind() == ErrorKind::NotFound && location.mode != ConfigMode::ExplicitRead =>
            {
                return Ok(Self::default());
            },
            Err(e) => return Err(e).context("Error reading Hub config file"),
        };

        let ConfigFile { hub } = toml::from_str(&text).context("Error parsing Hub config")?;

        Ok(hub.into_owned())
    }

    pub fn save(&self, location: &ConfigLocation) -> Result<()> {
        if location.mode == ConfigMode::Implicit {
            if let Some(dir) = location.path.parent() {
                fs::create_dir_all(dir).context("Error creating Hub config directory")?;
            }
        }

        let text = toml::to_string_pretty(&ConfigFile {
            hub: Cow::Borrowed(self),
        })
        .context("Error serializing Hub config")?;

        let swap = swap_path(location.path.clone()).context("Invalid Hub config path")?;

        fs::write(&swap, text.as_bytes()).context("Error writing Hub config swapfile")?;
        fs::rename(swap, &location.path).context("Error writing Hub config file")?;

        Ok(())
    }

    #[inline]
    pub fn set_graphql_endpoint(&mut self, endpoint: Url) { self.graphql_endpoint = endpoint; }

    #[inline]
    pub fn set_hub_endpoint(&mut self, endpoint: Option<Url>) { self.hub_endpoint = endpoint; }

    #[inline]
    pub fn set_token(&mut self, token: String) { self.token = token; }

    pub fn graphql_client_builder(&self) -> Result<reqwest::ClientBuilder> {
        Ok(reqwest::ClientBuilder::default()
            .gzip(true)
            .deflate(true)
            .brotli(true)
            .default_headers(
                [(
                    reqwest::header::AUTHORIZATION,
                    (&self.token)
                        .try_into()
                        .context("API token is not a valid HTTP header")?,
                )]
                .into_iter()
                .collect(),
            ))
    }

    #[inline]
    pub fn graphql_client(&self) -> Result<Client> {
        self.graphql_client_builder()?
            .build()
            .context("Error building HTTP client")
    }

    #[inline]
    pub fn post_graphql<'a, Q: graphql_client::GraphQLQuery + 'a>(
        &self,
        client: &'a Client,
        vars: Q::Variables,
    ) -> impl std::future::Future<
        Output = Result<graphql_client::Response<Q::ResponseData>, reqwest::Error>,
    > + 'a {
        graphql_client::reqwest::post_graphql::<Q, _>(client, self.graphql_endpoint.clone(), vars)
    }

    pub fn hub_endpoint(&self) -> Result<Cow<Url>> {
        self.hub_endpoint.as_ref().map(Cow::Borrowed).map_or_else(
            || {
                // TODO: try-block hack
                Some(())
                    .and_then(|()| {
                        let mut url = self.graphql_endpoint.clone();

                        let host = url.host_str()?;
                        let mut any_match = false;

                        let host = host
                            .split('.')
                            .map(|s| {
                                if !any_match && s == "api" {
                                    any_match = true;
                                    "hub"
                                } else {
                                    s
                                }
                            })
                            .join(".");

                        url.set_host(Some(any_match.then_some(host.as_ref())?))
                            .ok()?;

                        if let Some("graphql") = url.path_segments()?.last() {
                            url.path_segments_mut().unwrap().pop();
                        }

                        Some(Cow::Owned(url))
                    })
                    .context(
                        "Unable to infer Hub root endpoint from Hub GraphQL API endpoint, \
                         consider setting hub-endpoint in your config",
                    )
            },
            Ok,
        )
    }

    pub fn upload_endpoint(&self) -> Result<Url> {
        let mut url = self.hub_endpoint()?.into_owned();
        url.path_segments_mut()
            .map_err(|()| anyhow!("Invalid Hub endpoint"))?
            .push("uploads");
        Ok(url)
    }
}
