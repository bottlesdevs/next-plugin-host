mod bindings;
mod manifest;
mod runtime;

use async_trait::async_trait;
use url::Url;

pub use bindings::exports::bottles::plugin::{
    lifecycle::PluginKind,
    storefront_account_provider::{AccountIdentity, LinkedAccount},
    storefront_library_provider::{ListedGames, OwnedGame},
};
pub use manifest::{PluginManifest, parse_manifest};
pub use runtime::Plugin;

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginInfo {
    pub manifest: PluginManifest,
    pub provides: Vec<PluginKind>,
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
