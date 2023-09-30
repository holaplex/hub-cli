use serde::{Deserialize, Serialize};

use super::url_permissive::PermissiveUrl;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataJson {
    pub name: String,
    pub symbol: String,
    pub description: String,
    pub image: PermissiveUrl,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_url: Option<PermissiveUrl>,
    #[serde(default, skip_serializing_if = "Collection::is_empty")]
    pub collection: Collection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<Attribute>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_url: Option<PermissiveUrl>,
    #[serde(default, skip_serializing_if = "Properties::is_empty")]
    pub properties: Properties,
}

impl MetadataJson {
    pub fn files(&self) -> impl Iterator<Item = &PermissiveUrl> {
        [&self.image]
            .into_iter()
            .chain(self.animation_url.iter())
            .chain(self.external_url.iter())
            .chain(self.properties.files.iter().map(|f| &f.uri))
    }

    pub fn files_mut(&mut self) -> impl Iterator<Item = &mut PermissiveUrl> {
        [&mut self.image]
            .into_iter()
            .chain(self.animation_url.iter_mut())
            .chain(self.external_url.iter_mut())
            .chain(self.properties.files.iter_mut().map(|f| &mut f.uri))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Collection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
}

impl Collection {
    pub fn is_empty(&self) -> bool {
        let Self { name, family } = self;
        name.is_none() && family.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attribute {
    pub trait_type: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Properties {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<File>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

impl Properties {
    pub fn is_empty(&self) -> bool {
        let Self { files, category } = self;
        files.is_empty() && category.is_none()
    }

    pub fn find_file<U>(&self, url: &U) -> Option<&File>
    where PermissiveUrl: PartialEq<U> {
        self.files.iter().find(|f| f.uri == *url)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct File {
    pub uri: PermissiveUrl,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub ty: Option<String>,
}
