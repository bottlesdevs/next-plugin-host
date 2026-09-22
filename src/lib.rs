mod inspection;
mod manifest;
mod packages;
mod runtime;

mod interfaces {
    include!(concat!(env!("OUT_DIR"), "/plugin_interfaces.rs"));
}

pub use inspection::exported_interfaces;
pub use interfaces::PluginInterface;
pub use manifest::{PluginManifest, parse_manifest};
pub use packages::{LoadedPlugin, Plugins};
pub use runtime::{HostState, Invocation, Runtime};

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("package index: {0}")]
    Index(#[from] next_config::error::Error),
    #[error("invalid component: {0}")]
    Component(#[from] wasmparser::BinaryReaderError),
    #[error("plugin manifest schema {0} is not supported")]
    UnsupportedSchema(u32),
    #[error("failed to parse plugin manifest: {0}")]
    ParseManifest(#[from] toml::de::Error),
    #[error("plugin {0} was not found")]
    NotFound(String),
    #[error("plugin runtime failed: {0}")]
    Runtime(#[from] wasmtime::Error),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginInfo {
    pub revision: uuid::Uuid,
    pub manifest: PluginManifest,
    pub interfaces: Vec<String>,
}

impl PluginInfo {
    /// Reports export presence; typed binding checks compatibility on invocation.
    pub fn exports(&self, interface: PluginInterface) -> bool {
        self.interfaces
            .iter()
            .any(|name| name == interface.as_str())
    }
}

pub type Result<T> = std::result::Result<T, PluginError>;
