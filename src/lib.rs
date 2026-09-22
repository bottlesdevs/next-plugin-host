mod manifest;
mod packages;
mod runtime;
pub mod storefront;
pub use storefront::{AccountIdentity, Authentication, LinkedAccount, OwnedGame};

mod interfaces {
    include!(concat!(env!("OUT_DIR"), "/plugin_interfaces.rs"));
}

pub use interfaces::PluginInterface;
pub use manifest::{PluginManifest, parse_manifest};
pub use packages::{LoadedPlugin, Plugins};
pub(crate) use runtime::{HostState, Invocation, Runtime};

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("failed to serialize package index: {0}")]
    SerializeIndex(#[from] toml::ser::Error),
    #[error("plugin manifest schema {0} is not supported")]
    UnsupportedSchema(u32),
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
    pub(crate) revision: uuid::Uuid,
    pub manifest: PluginManifest,
    pub(crate) interfaces: Vec<String>,
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

/// Input capability supplied by the application to one account-link invocation.
#[async_trait::async_trait]
pub trait AccountLinkInteraction: Send + Sync {
    async fn request_input(
        &self,
        url: url::Url,
        instructions: String,
    ) -> std::result::Result<String, String>;
}
