mod bindings;
mod runtime;

use async_trait::async_trait;
use url::Url;

pub use bindings::exports::bottles::plugin::{
    lifecycle::PluginKind,
    storefront_account_provider::{AccountIdentity, LinkedAccount},
    storefront_library_provider::{ListedGames, OwnedGame},
};
pub use runtime::Plugin;

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
