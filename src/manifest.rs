use serde::Deserialize;
use std::{fs, path::Path};

/// Identity and display metadata shipped beside a plugin component.
///
/// Read from `plugin.toml` by [`crate::Plugins`]. The ID is used for package
/// paths and core provider registration; the version is display metadata,
/// separate from the versioned WIT interface names.
///
/// # Examples
///
/// ```text
/// let manifest = bottles_plugin_host::Manifest {
///     id: "example".into(),
///     name: "Example provider".into(),
///     version: "0.1.0".into(),
/// };
/// ```
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Manifest {
    /// Package identifier used directly in catalog paths and provider IDs.
    pub id: String,
    /// Display name used in account provider metadata.
    pub name: String,
    /// Display version, independent of WIT interface versions.
    pub version: String,
}

impl Manifest {
    pub(crate) fn read(path: &Path) -> wasmtime::Result<Self> {
        Ok(toml::from_str(&fs::read_to_string(path)?)?)
    }
}
