use std::{io, path::PathBuf};
use uuid::NonNilUuid;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read plugin manifest {path}: {source}")]
    ReadManifest { path: PathBuf, source: io::Error },
    #[error("invalid plugin manifest: {0}")]
    InvalidManifest(String),
    #[error("failed to parse plugin manifest: {0}")]
    ParseManifest(#[from] toml::de::Error),
    #[error("plugin {0:?} was not found")]
    PluginNotFound(NonNilUuid),
    #[error("plugin API version {actual} is not supported; expected {expected}")]
    ApiVersion { actual: String, expected: String },
    #[error("plugin component is invalid: {0}")]
    InvalidComponent(String),
    #[error("plugin {0:?} failed to load: {1}")]
    Load(String, String),
    #[error("host operation failed: {0}")]
    Host(String),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
