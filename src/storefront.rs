use crate::AccountLinkInteraction;
use crate::{HostState, Invocation, LoadedPlugin};
use std::sync::Arc;
use wasmtime::component::{HasSelf, Linker, Resource};

mod bindings {
    pub type AccountLinkInteractionResource = Arc<dyn super::AccountLinkInteraction>;
    use std::sync::Arc;
    wasmtime::component::bindgen!({
        path: "../next-plugin-api/wit",
        world: "storefront",
        additional_derives: [serde::Serialize, serde::Deserialize, PartialEq, Eq],
        imports: { default: async | trappable },
        exports: { default: async },
        with: {
            "bottles:plugin/account-link.interaction": AccountLinkInteractionResource,
        },
    });
}

use bindings::{bottles::plugin::account_link, exports::bottles::plugin::storefront_provider};
pub use storefront_provider::{AccountIdentity, Authentication, LinkedAccount, OwnedGame};
type Result<T> = std::result::Result<T, String>;

/// Register storefront imports once when composing the application runtime.
pub fn add_plugin_imports(linker: &mut Linker<HostState>) -> wasmtime::Result<()> {
    account_link::add_to_linker::<_, HasSelf<_>>(linker, |state| state)
}

impl account_link::HostInteraction for HostState {
    async fn request_input(
        &mut self,
        interaction: Resource<Arc<dyn AccountLinkInteraction>>,
        url: String,
        instructions: String,
    ) -> wasmtime::Result<std::result::Result<String, String>> {
        let interaction = self.table.get(&interaction)?.clone();
        let url = match url::Url::parse(&url) {
            Ok(url) => url,
            Err(error) => return Ok(Err(format!("invalid interaction URL: {error}"))),
        };
        Ok(interaction.request_input(url, instructions).await)
    }

    async fn drop(
        &mut self,
        interaction: Resource<Arc<dyn AccountLinkInteraction>>,
    ) -> wasmtime::Result<()> {
        self.table.delete(interaction)?;
        Ok(())
    }
}
impl account_link::Host for HostState {}

pub async fn link_account(
    plugin: &LoadedPlugin,
    interaction: Arc<dyn AccountLinkInteraction>,
) -> Result<LinkedAccount> {
    let component = &plugin.component;
    let indices = storefront_provider::GuestIndices::new(component).map_err(|e| e.to_string())?;
    let mut invocation = Invocation::new(component)
        .await
        .map_err(|e| e.to_string())?;
    let guest = indices
        .load(&mut invocation.store, &invocation.instance)
        .map_err(|e| e.to_string())?;
    let interaction = invocation
        .store
        .data_mut()
        .table
        .push(interaction)
        .map_err(|e| e.to_string())?;
    let borrowed = Resource::new_borrow(interaction.rep());
    guest
        .call_link_account(&mut invocation.store, borrowed)
        .await
        .map_err(|e| e.to_string())?
}

pub async fn authenticate(
    plugin: &LoadedPlugin,
    account_id: &str,
    credential: Option<&[u8]>,
) -> Result<Authentication> {
    let component = &plugin.component;
    let indices = storefront_provider::GuestIndices::new(component).map_err(|e| e.to_string())?;
    let mut invocation = Invocation::new(component)
        .await
        .map_err(|e| e.to_string())?;
    let guest = indices
        .load(&mut invocation.store, &invocation.instance)
        .map_err(|e| e.to_string())?;
    guest
        .call_authenticate(&mut invocation.store, account_id, credential)
        .await
        .map_err(|e| e.to_string())?
}

pub async fn list_games(
    plugin: &LoadedPlugin,
    account_id: &str,
    access: &[u8],
) -> Result<Vec<OwnedGame>> {
    let component = &plugin.component;
    let indices = storefront_provider::GuestIndices::new(component).map_err(|e| e.to_string())?;
    let mut invocation = Invocation::new(component)
        .await
        .map_err(|e| e.to_string())?;
    let guest = indices
        .load(&mut invocation.store, &invocation.instance)
        .map_err(|e| e.to_string())?;
    guest
        .call_list_games(&mut invocation.store, account_id, access)
        .await
        .map_err(|e| e.to_string())?
}
