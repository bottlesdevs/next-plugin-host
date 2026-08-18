mod storefront;

use std::sync::Arc;

use bottles_core::{Bottles, Profiles};
use uuid::NonNilUuid;

use crate::runtime::PluginHandle;

use storefront::StorefrontAccountAdapter;

/// Connects manifest contributions to their native subsystem registries.
pub(crate) struct PluginAdapters {
    profiles: Profiles,
}

impl PluginAdapters {
    pub(crate) fn new(bottles: &Bottles) -> Self {
        Self {
            profiles: bottles.profiles().clone(),
        }
    }

    /// Registers the contributions declared by a loaded plugin.
    pub(crate) fn register(&self, handle: Arc<PluginHandle>) {
        let plugin_id = handle.manifest().id;
        if handle.manifest().storefront_account {
            self.profiles
                .register_account_provider(Arc::new(StorefrontAccountAdapter::new(handle)));
        } else {
            self.unregister(plugin_id);
        }
    }

    /// Removes every native adapter owned by a plugin.
    pub(crate) fn unregister(&self, plugin_id: NonNilUuid) {
        self.profiles.unregister_account_provider(plugin_id);
    }
}
