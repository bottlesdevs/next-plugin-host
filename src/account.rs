use std::sync::Arc;

use async_trait::async_trait;
use bottles_core::{
    AccountIdentity, AccountLinkInteraction, AccountProvider, AccountProviderInfo, LinkedAccount,
};
use wasmtime::component::{Accessor, HasSelf, Linker, Resource};

use crate::{
    Capability, PluginInterface,
    plugin::{Plugin, WasiState, call},
};

/// Marker for plugins that export the account-provider interface.
///
/// [`Plugin<Account>`] implements [`bottles_core::AccountProvider`]. Its metadata
/// comes from the manifest. Account linking forwards input requests to the
/// caller's [`AccountLinkInteraction`] and returns the guest's identity and
/// optional credential unchanged.
///
/// The interaction remains in the driver's resource table until the queued
/// invocation finishes, even if its caller stops awaiting it. The resource is
/// removed after the guest returns, including when the call returns an error;
/// shutting down the driver drops its table and any remaining interactions.
/// Binding, driver, and guest failures are returned as error strings.
///
/// # Examples
///
/// ```text
/// if let Some(account) = plugin.cast::<bottles_plugin_host::Account>() {
///     bottles.profiles().register_provider(std::sync::Arc::new(account));
/// }
/// ```
pub struct Account;

impl Capability for Account {
    const INTERFACE: PluginInterface = PluginInterface::AccountProvider;

    fn new(_: &Plugin) -> Self {
        Account
    }
}

mod bindings {
    pub type Interaction = std::sync::Arc<dyn bottles_core::AccountLinkInteraction>;

    wasmtime::component::bindgen!({
        path: "../next-plugin-api/wit",
        world: "account",
        imports: { default: trappable },
        exports: { default: async | store },
        with: { "bottles:plugin/account-link.interaction": Interaction },
    });
}

use bindings::bottles::plugin::account_link;

pub(crate) fn add_to_linker(linker: &mut Linker<WasiState>) -> wasmtime::Result<()> {
    account_link::add_to_linker::<_, HasSelf<WasiState>>(linker, |state| state)
}

impl<T: Send + 'static> account_link::HostInteractionWithStore<T> for HasSelf<WasiState> {
    async fn request_input(
        accessor: &Accessor<T, Self>,
        interaction: Resource<Arc<dyn AccountLinkInteraction>>,
        url: String,
        instructions: String,
    ) -> wasmtime::Result<Result<String, String>> {
        let interaction =
            accessor.with(|mut access| access.get().table.get(&interaction).cloned())?;
        let url = match url::Url::parse(&url) {
            Ok(url) => url,
            Err(error) => return Ok(Err(error.to_string())),
        };
        Ok(interaction.request_input(url, instructions).await)
    }
}

impl account_link::HostInteraction for WasiState {
    fn drop(
        &mut self,
        interaction: Resource<Arc<dyn AccountLinkInteraction>>,
    ) -> wasmtime::Result<()> {
        self.table.delete(interaction)?;
        Ok(())
    }
}

impl account_link::Host for WasiState {}

#[async_trait]
impl AccountProvider for Plugin<Account> {
    fn metadata(&self) -> AccountProviderInfo {
        AccountProviderInfo {
            id: self.manifest().id.clone(),
            name: self.manifest().name.clone().into(),
        }
    }

    async fn link_account(
        &self,
        interaction: Arc<dyn AccountLinkInteraction>,
    ) -> Result<LinkedAccount, String> {
        call(&self.shared.calls, move |accessor, instance| {
            Box::pin(async move {
                let bindings =
                    accessor.with(|mut access| bindings::Account::new(&mut access, instance))?;
                let interaction =
                    accessor.with(|mut access| access.get().table.push(interaction))?;
                let borrowed = Resource::new_borrow(interaction.rep());
                let result = bindings
                    .bottles_plugin_account_provider()
                    .call_link_account(accessor, borrowed)
                    .await;
                accessor.with(|mut access| access.get().table.delete(interaction))?;
                result
            })
        })
        .await
        .map_err(|error| error.to_string())?
        .map(|linked| LinkedAccount {
            identity: AccountIdentity {
                account_id: linked.identity.account_id,
                display_name: linked.identity.display_name,
            },
            credential: linked.credential,
        })
    }
}
