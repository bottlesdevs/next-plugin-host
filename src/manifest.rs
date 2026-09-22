use semver::Version;
use serde::{Deserialize, Serialize};

use crate::PluginError;

/// Identity and display metadata shipped beside a plugin component.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginManifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: Version,
    pub description: String,
    pub authors: Vec<String>,
    pub license: String,
    pub repository: url::Url,
}

pub fn parse_manifest(source: &str) -> Result<PluginManifest, PluginError> {
    Ok(toml::from_str(source)?)
}
