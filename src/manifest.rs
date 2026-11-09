use serde::Deserialize;
use std::{fs, path::Path};
use wasmtime::error::Context;

/// Identity and display metadata shipped beside a plugin component.
///
/// Read from `plugin.toml` by [`crate::Plugins`]. The ID is used for package
/// paths and to identify the plugin to callers; the version is display metadata,
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
    /// Reads the manifest of the package in `package`.
    pub(crate) fn read(package: &Path) -> wasmtime::Result<Self> {
        let path = crate::manifest_path(package);
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read plugin manifest {}", path.display()))?;

        toml::from_str(&source)
            .with_context(|| format!("failed to read plugin manifest {}", path.display()))
    }
}
