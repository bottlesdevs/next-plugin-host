use std::path::Path;

use semver::Version;
use serde::{Deserialize, Serialize};
use uuid::NonNilUuid;

use crate::{Error, Result};

pub(crate) const MANIFEST_FILE: &str = "plugin.toml";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginManifest {
    pub schema_version: u32,
    pub id: NonNilUuid,
    pub name: String,
    pub version: Version,
    pub description: String,
    pub authors: Vec<String>,
    pub license: String,
    pub repository: url::Url,
    pub api_version: Version,
}

impl PluginManifest {
    pub async fn load(directory: &Path) -> Result<Self> {
        let path = directory.join(MANIFEST_FILE);
        let source = async_fs::read_to_string(&path)
            .await
            .map_err(|source| Error::ReadManifest { path, source })?;
        Self::parse(&source)
    }

    pub fn parse(source: &str) -> Result<Self> {
        Ok(toml::from_str(source)?)
    }
}
