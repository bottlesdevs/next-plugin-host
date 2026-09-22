mod bindings;
mod inspection;
mod manifest;
mod packages;
mod runtime;
pub mod storefront;

mod interfaces {
    include!(concat!(env!("OUT_DIR"), "/plugin_interfaces.rs"));
}

use async_trait::async_trait;
use url::Url;

pub use bindings::exports::bottles::plugin::storefront_provider::{
    AccountIdentity, Authentication, LinkedAccount, OwnedGame,
};
pub use inspection::exported_interfaces;
pub use interfaces::PluginInterface;
pub use manifest::{PluginManifest, parse_manifest};
pub use packages::{LoadedPlugin, Plugins};
pub use runtime::{CompiledPlugin, Runtime};

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
    Runtime(String),
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

pub type Result<T> = std::result::Result<T, String>;

/// A host-owned interaction used by account-provider plugins to ask the user
/// for a value, such as a browser callback URL or authorization code.
/// The host parses the component's URL before invoking this callback.
#[async_trait]
pub trait AccountLinkInteraction: Send + Sync {
    async fn request_input(
        &self,
        url: Url,
        instructions: String,
    ) -> std::result::Result<String, String>;
}
