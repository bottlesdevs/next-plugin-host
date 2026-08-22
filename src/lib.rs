mod bindings;
mod runtime;

use async_trait::async_trait;

pub use bindings::exports::bottles::plugin::{
    lifecycle::PluginKind,
    storefront_account_provider::{AccountIdentity, LinkedAccount},
};
pub use runtime::Plugin;

pub type Result<T> = std::result::Result<T, String>;

/// A host-owned interaction used by account-provider plugins to ask the user
/// for a value, such as a browser callback URL or authorization code.
#[async_trait]
pub trait AccountLinkInteraction: Send + Sync {
    async fn request_input(
        &self,
        url: String,
        instructions: String,
    ) -> std::result::Result<String, String>;
}
