use std::{fmt, path::PathBuf, str::FromStr};

use serde::{
    de::{self, Visitor},
    Deserialize, Serialize,
};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PermissiveUrl {
    Url(Url),
    Path(PathBuf),
}

impl PermissiveUrl {
    pub fn to_file_path(&self) -> Option<PathBuf> {
        match self {
            Self::Url(u) => u.to_file_path().ok(),
            Self::Path(p) => Some(p.clone()),
        }
    }
}

impl From<PermissiveUrl> for String {
    #[inline]
    fn from(val: PermissiveUrl) -> Self {
        match val {
            PermissiveUrl::Url(u) => u.into(),
            PermissiveUrl::Path(p) => p.to_string_lossy().into_owned(),
        }
    }
}

impl FromStr for PermissiveUrl {
    type Err = url::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Url::parse(s)
            .map(Self::Url)
            .or_else(|e| PathBuf::from_str(s).map(Self::Path).map_err(|_| e))
    }
}

impl<Rhs> PartialEq<Rhs> for PermissiveUrl
where
    Url: PartialEq<Rhs>,
    PathBuf: PartialEq<Rhs>,
{
    fn eq(&self, other: &Rhs) -> bool {
        match self {
            PermissiveUrl::Url(u) => u.eq(other),
            PermissiveUrl::Path(p) => p.eq(other),
        }
    }
}

impl Serialize for PermissiveUrl {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where S: serde::Serializer {
        match self {
            PermissiveUrl::Url(u) => u.serialize(serializer),
            PermissiveUrl::Path(p) => p.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for PermissiveUrl {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where D: serde::Deserializer<'de> {
        struct PermissiveUrlVisitor;

        impl<'de> Visitor<'de> for PermissiveUrlVisitor {
            type Value = PermissiveUrl;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a string representing an URL")
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where E: de::Error {
                PermissiveUrl::from_str(s).map_err(|err| {
                    de::Error::invalid_value(de::Unexpected::Str(s), &err.to_string().as_str())
                })
            }
        }

        deserializer.deserialize_str(PermissiveUrlVisitor)
    }
}
