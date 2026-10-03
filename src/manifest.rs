use serde::Deserialize;
use std::{fs, path::Path};

/// Identity and display metadata shipped beside a plugin component.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Manifest {
    /// Stable package identifier and directory name.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Display version.
    pub version: String,
}

impl Manifest {
    pub(crate) fn read(path: &Path) -> wasmtime::Result<Self> {
        Ok(toml::from_str(&fs::read_to_string(path)?)?)
    }
}
