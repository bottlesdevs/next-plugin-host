use std::sync::Arc;

use async_trait::async_trait;
use wasmtime::component::{Accessor, HasSelf, Linker, Resource};

use crate::{
    Capability, interfaces,
    plugin::{Plugin, WasiState},
};

/// Supplies user input requested by a plugin during account linking.
#[async_trait]
pub trait Interaction: Send + Sync {
    /// Shows `url` with `instructions` and returns the user's input.
    async fn request_input(&self, url: url::Url, instructions: String) -> Result<String, String>;
}

/// Marker for plugins that export the account-provider interface.
///
/// [`Plugin<Account>`] links an account by forwarding the guest's input requests
/// to the caller's [`Interaction`] and returns the guest's identity unchanged.
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
///     let identity = account.link_account(interaction).await?;
///     println!("{}", identity.display_name);
/// }
/// ```
pub struct Account;

impl Capability for Account {
    const INTERFACE: &'static str = interfaces::ACCOUNT_PROVIDER;
}

#[allow(missing_docs)]
mod bindings {
    pub type Interaction = std::sync::Arc<dyn super::Interaction>;

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
        interaction: Resource<Arc<dyn Interaction>>,
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
    fn drop(&mut self, interaction: Resource<Arc<dyn Interaction>>) -> wasmtime::Result<()> {
        self.table.delete(interaction)?;
        Ok(())
    }
}

impl account_link::Host for WasiState {}

/// Public identity reported by an account plugin.
pub use bindings::exports::bottles::plugin::account_provider::AccountIdentity;

impl Plugin<Account> {
    /// Runs the guest's account-link flow using `interaction` for user input.
    ///
    /// # Errors
    ///
    /// Returns an error if the driver is closed, the bindings cannot be
    /// constructed, or the guest reports a failure.
    pub async fn link_account(
        &self,
        interaction: Arc<dyn Interaction>,
    ) -> wasmtime::Result<AccountIdentity> {
        self.call(move |accessor, instance| {
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
        .await?
        .map_err(wasmtime::Error::msg)
    }
}
