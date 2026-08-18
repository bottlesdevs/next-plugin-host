use std::sync::Arc;

use async_trait::async_trait;
use bottles_core::{AccountIdentity, StorefrontAccountProvider, StorefrontProvider};
use uuid::Uuid;

use crate::runtime::PluginHandle;

/// Adapts one manifest-declared plugin to the native profile account contract.
pub(crate) struct StorefrontAccountAdapter {
    handle: Arc<PluginHandle>,
}

impl StorefrontAccountAdapter {
    pub(crate) fn new(handle: Arc<PluginHandle>) -> Self {
        Self { handle }
    }
}

#[async_trait]
impl StorefrontAccountProvider for StorefrontAccountAdapter {
    fn provider(&self) -> StorefrontProvider {
        StorefrontProvider {
            id: self.handle.manifest().id,
            name: self.handle.manifest().name.clone(),
        }
    }

    async fn link_account(&self, profile_id: Uuid) -> Result<AccountIdentity, String> {
        self.handle
            .link_account(profile_id.to_string())
            .await
            .map_err(|error| error.to_string())
    }
}
