use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{anyhow, Context, Result};
use directories::ProjectDirs;
use log::warn;
use reqwest::Client;
use toml_edit::{Document, Item, Table};
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

#[derive(Debug, Clone, Copy)]
enum ConfigMode {
    ExplicitWrite,
    ExplicitRead,
    Implicit(bool),
}

impl ConfigMode {
    fn writable(self) -> bool {
        match self {
            Self::ExplicitWrite => true,
            Self::ExplicitRead => false,
            Self::Implicit(w) => w,
        }
    }
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
            (None, w) => ConfigMode::Implicit(w),
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

mod dirty {
    use std::{mem, ops};

    use toml_edit::Value;

    #[derive(Debug, Clone, Copy, Default)]
    pub struct Dirty<T> {
        dirty: bool,
        value: T,
    }

    impl<T> From<T> for Dirty<T> {
        #[inline]
        fn from(value: T) -> Self {
            Self {
                dirty: false,
                value,
            }
        }
    }

    impl<T> ops::Deref for Dirty<T> {
        type Target = T;

        #[inline]
        fn deref(&self) -> &Self::Target { &self.value }
    }

    impl<T> ops::DerefMut for Dirty<T> {
        #[inline]
        fn deref_mut(&mut self) -> &mut Self::Target {
            self.dirty = true;
            &mut self.value
        }
    }

    impl<T> Dirty<T> {
        pub fn write_entry<'a, 'b, U: Into<Value> + 'b, F: FnOnce(&'b T) -> U>(
            &'b mut self,
            item: toml_edit::Entry<'a>,
            map: F,
        ) where
            T: 'b,
        {
            use toml_edit::Entry::{Occupied, Vacant};

            let true = mem::replace(&mut self.dirty, false) else {
                return;
            };

            let val = toml_edit::value(map(&self.value));
            match item {
                Occupied(o) => {
                    *o.into_mut() = val;
                },
                Vacant(v) => {
                    v.insert(val);
                },
            }
        }
    }
}

use dirty::Dirty;

#[derive(Debug, Clone)]
pub struct Config {
    doc: Document,
    api_endpoint: Dirty<Url>,
    token: Dirty<String>,
}

impl Config {
    const DEFAULT_TOKEN: &'static str = "";

    fn default_api_endpoint() -> &'static Url {
        static ENDPOINT: OnceLock<Url> = OnceLock::new();
        ENDPOINT.get_or_init(|| "https://api.holaplex.com/".try_into().unwrap())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            doc: Document::default(),
            api_endpoint: Self::default_api_endpoint().clone().into(),
            token: Self::DEFAULT_TOKEN.to_owned().into(),
        }
    }
}

impl Config {
    const ROOT_NAME: &'static str = "hub";
    const API_ENDPOINT_NAME: &'static str = "api-endpoint";
    const TOKEN_NAME: &'static str = "token";

    fn guess_api_from_graphql(url: &str) -> Option<Url> {
        let mut url = Url::parse(url).ok()?;
        let segs = url.path_segments()?;

        if segs.last()? != "graphql" {
            return None;
        }

        url.path_segments_mut().ok()?.pop();

        Some(url)
    }

    pub fn load(location: &ConfigLocation) -> Result<Self> {
        let text = match fs::read_to_string(&location.path) {
            Ok(s) => s,
            Err(e)
                if e.kind() == ErrorKind::NotFound && !matches!(location.mode, ConfigMode::ExplicitRead) =>
            {
                return Ok(Self::default());
            },
            Err(e) => return Err(e).context("Error reading Hub config file"),
        };

        let doc: Document = text.parse().context("Error parsing Hub config")?;

        // TODO: try-block hack
        Ok(doc)
            .and_then(|mut doc| {
                let mut api_endpoint = None;
                let mut api_guess = None;
                let mut token = None;

                if let Some(root) = doc.get_mut(Self::ROOT_NAME) {
                    let root = root.as_table_mut().ok_or("Expected [hub] to be a table")?;

                    if let Some(e) = root.get(Self::API_ENDPOINT_NAME) {
                        api_endpoint = Some(
                            e.as_str()
                                .and_then(|e| Url::parse(e).ok())
                                .ok_or("Expected hub.api-endpoint to be a valid URL")?,
                        );
                    }

                    if let Some(t) = root.get(Self::TOKEN_NAME) {
                        token = Some(
                            t.as_str()
                                .ok_or("Expected hub.token to be a string")?
                                .to_owned(),
                        );
                    }

                    {
                        let no_write = !location.mode.writable();

                        if let Some(e) = root.remove("graphql-endpoint") {
                            if no_write {
                                warn!(
                                    "The graphql-endpoint configuration property is deprecated.  \
                                     Run `hub config update` to update your configuration file."
                                );
                            }

                            let endpoint = e.as_str().and_then(Self::guess_api_from_graphql);

                            match (endpoint, &api_endpoint) {
                                (Some(e), Some(a)) if e == *a => (),
                                (Some(_), Some(_)) => warn!(
                                    "Conflicting API endpoints defined by graphql-endpoint and \
                                     api-endpoint in your config.  Ignoring graphql-endpoint \
                                     property."
                                ),
                                (e, None) => api_guess = e,
                                (None, _) => warn!(
                                    "Unable to infer api-endpoint property from graphql-endpoint \
                                     property.  Ignoring graphql-endpoint."
                                ),
                            }
                        }

                        if root.remove("hub-endpoint").is_some() && no_write {
                            warn!(
                                "The hub-endpoint configuration property is deprecated and will \
                                 be ignored.  Run `hub config update` to update your \
                                 configuration file."
                            );
                        }
                    }
                }

                let mut api_endpoint: Dirty<_> = api_endpoint
                    .unwrap_or_else(|| Self::default_api_endpoint().clone())
                    .into();

                if let Some(guess) = api_guess {
                    *api_endpoint = guess;
                }

                Ok(Self {
                    doc,
                    api_endpoint,
                    token: token.unwrap_or_else(|| Self::DEFAULT_TOKEN.into()).into(),
                })
            })
            .map_err(|e: &str| anyhow!("Error parsing Hub config: {e}"))
    }

    pub fn save(&mut self, location: &ConfigLocation) -> Result<()> {
        assert!(location.mode.writable(), "Config::save called with a non-writable location!");

        if matches!(location.mode, ConfigMode::Implicit(_)) {
            if let Some(dir) = location.path.parent() {
                fs::create_dir_all(dir).context("Error creating Hub config directory")?;
            }
        }

        // TODO: try-block hack
        Ok(())
            .and_then(|()| {
                let Self {
                    doc,
                    api_endpoint,
                    token,
                } = self;

                let root = doc
                    .entry(Self::ROOT_NAME)
                    .or_insert(Item::Table(Table::default()))
                    .as_table_mut()
                    .ok_or("Expected [hub] to be a table")?;

                api_endpoint.write_entry(root.entry(Self::API_ENDPOINT_NAME), Url::as_str);
                token.write_entry(root.entry(Self::TOKEN_NAME), |u| u);

                Ok(())
            })
            .map_err(|e: &str| anyhow!("Error updating Hub config: {e}"))?;

        let swap = swap_path(location.path.clone()).context("Invalid Hub config path")?;

        fs::write(&swap, self.doc.to_string()).context("Error writing Hub config swapfile")?;
        fs::rename(swap, &location.path).context("Error writing Hub config file")?;

        Ok(())
    }

    #[inline]
    pub fn set_api_endpoint(&mut self, endpoint: Url) { *self.api_endpoint = endpoint; }

    #[inline]
    pub fn set_token(&mut self, token: String) { *self.token = token; }

    pub fn api_client_builder(&self) -> Result<reqwest::ClientBuilder> {
        Ok(reqwest::ClientBuilder::default()
            .gzip(true)
            .deflate(true)
            .brotli(true)
            .default_headers(
                [(
                    reqwest::header::AUTHORIZATION,
                    (&*self.token)
                        .try_into()
                        .context("API token is not a valid HTTP header")?,
                )]
                .into_iter()
                .collect(),
            ))
    }

    #[inline]
    pub fn api_client(&self) -> Result<Client> {
        self.api_client_builder()?
            .build()
            .context("Error building HTTP client")
    }

    #[inline]
    pub fn api_endpoint(&self) -> &Url { &self.api_endpoint }

    pub fn graphql_endpoint(&self) -> Result<Url> {
        self.api_endpoint
            .join("graphql")
            .context("Invalid Hub API endpoint")
    }

    pub fn upload_endpoint(&self) -> Result<Url> {
        self.api_endpoint
            .join("uploads")
            .context("Invalid Hub API endpoint")
    }
}
