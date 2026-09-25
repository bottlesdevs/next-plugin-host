mod manifest;
mod packages;
mod runtime;

pub use manifest::{PluginManifest, parse_manifest};
pub use packages::{CompiledPlugin, Plugins};
pub(crate) use runtime::Runtime;
pub use runtime::{Invocation, Session, WasiState, add_to_linker};

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("failed to serialize plugin metadata: {0}")]
    SerializeMetadata(#[from] toml::ser::Error),
    #[error("failed to parse plugin metadata: {0}")]
    ParseMetadata(#[from] toml::de::Error),
    #[error("plugin {0} was not found")]
    NotFound(String),
    #[error("plugin runtime failed: {0}")]
    Runtime(#[from] wasmtime::Error),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginInfo {
    #[serde(flatten)]
    pub manifest: PluginManifest,
    pub(crate) interfaces: Vec<String>,
}

impl PluginInfo {
    /// Reports export presence; typed binding checks compatibility on invocation.
    pub fn exports(&self, interface: impl AsRef<str>) -> bool {
        let interface = interface.as_ref();
        self.interfaces.iter().any(|name| name == interface)
    }
}

pub type Result<T> = std::result::Result<T, PluginError>;
