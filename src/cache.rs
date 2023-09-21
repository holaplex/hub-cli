use std::{
    fs::{self, File},
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const SWAPFILE_NAME: &str = ".hub.cache~";
const FILE_NAME: &str = ".hub.cache";

fn swap_file(path: impl AsRef<Path>) -> PathBuf { path.as_ref().join(SWAPFILE_NAME) }
fn cache_file(path: impl AsRef<Path>) -> PathBuf { path.as_ref().join(FILE_NAME) }

#[derive(Debug, Default)]
#[repr(transparent)]
struct CacheItem<T>(Option<T>);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for CacheItem<T> {
    #[inline]
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where D: serde::Deserializer<'de> {
        Deserialize::deserialize(deserializer).map(Self)
    }

    #[inline]
    fn deserialize_in_place<D>(
        deserializer: D,
        place: &mut Self,
    ) -> std::result::Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Deserialize::deserialize_in_place(deserializer, &mut place.0)
    }
}

impl<T: Default + Eq + Serialize> Serialize for CacheItem<T> {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where S: serde::Serializer {
        Serialize::serialize(
            if self.0.as_ref().map_or(false, |v| *v == T::default()) {
                &None
            } else {
                &self.0
            },
            serializer,
        )
    }
}

impl<T: Default> CacheItem<T> {
    fn as_mut(&mut self) -> &mut T {
        if let Some(ref mut v) = self.0 {
            v
        } else {
            self.0 = Some(T::default());
            self.0.as_mut().unwrap()
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Cache {
    asset_uploads: CacheItem<AssetUploads>,
}

impl Cache {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = cache_file(&path);
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("Error opening cache file {path:?}")),
        };

        ron::de::from_reader(file).with_context(|| format!("Error parsing cache file {path:?}"))
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let swap = swap_file(&path);
        let path = cache_file(path);
        let file = File::create(&swap)
            .with_context(|| format!("Error creating cache swapfile {swap:?}"))?;

        ron::ser::to_writer(file, &self)
            .with_context(|| format!("Error serializing cache to {swap:?}"))?;

        fs::rename(swap, &path).with_context(|| format!("Error writing to cache file {path:?}"))
    }

    #[inline]
    pub fn asset_uploads_mut(&mut self) -> &mut AssetUploads { self.asset_uploads.as_mut() }
}

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetUploads {}
