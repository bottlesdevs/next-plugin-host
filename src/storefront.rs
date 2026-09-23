use crate::AccountLinkInteraction;
use crate::{HostState, LoadedPlugin};
use std::sync::Arc;
use wasmtime::component::{HasSelf, Linker, Resource};

mod bindings {
    pub type AccountLinkInteractionResource = Arc<dyn super::AccountLinkInteraction>;
    use std::sync::Arc;
    wasmtime::component::bindgen!({
        path: "../next-plugin-api/wit",
        world: "account",
        additional_derives: [serde::Serialize, serde::Deserialize, PartialEq, Eq],
        imports: { default: async | trappable },
        exports: { default: async },
        with: {
            "bottles:plugin/account-link.interaction": AccountLinkInteractionResource,
        },
    });
}

pub use account_provider::{AccountIdentity, LinkedAccount};
use bindings::{bottles::plugin::account_link, exports::bottles::plugin::account_provider};
type Result<T> = std::result::Result<T, String>;

pub(crate) fn add_plugin_imports(linker: &mut Linker<HostState>) -> wasmtime::Result<()> {
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
    plugin
        .worker
        .call(move |invocation| {
            Box::pin(async move {
                let guest = match account_provider::GuestIndices::new(&invocation.component)
                    .and_then(|indices| indices.load(&mut invocation.store, &invocation.instance))
                {
                    Ok(guest) => guest,
                    Err(error) => return Ok(Err(error.to_string())),
                };
                let interaction = invocation.store.data_mut().table.push(interaction)?;
                let borrowed = Resource::new_borrow(interaction.rep());
                let result = guest
                    .call_link_account(&mut invocation.store, borrowed)
                    .await?;
                invocation.store.data_mut().table.delete(interaction)?;
                Ok(result)
            })
        })
        .await
        .map_err(|error| error.to_string())?
}
