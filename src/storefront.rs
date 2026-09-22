use crate::runtime::{HostState, Invocation};
use crate::{
    AccountLinkInteraction, Authentication, CompiledPlugin, LinkedAccount, OwnedGame, Result,
    bindings::{bottles::plugin::account_link, exports::bottles::plugin::storefront_provider},
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use wasmtime::component::{HasSelf, InstancePre, Resource};

fn bind(component: &CompiledPlugin) -> Result<InstancePre<HostState>> {
    let mut linker = component.linker()?;
    account_link::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
        .map_err(|e| e.to_string())?;
    linker
        .instantiate_pre(&component.component)
        .map_err(|e| e.to_string())
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
    component: &CompiledPlugin,
    interaction: Arc<dyn AccountLinkInteraction>,
    cancellation: &CancellationToken,
) -> Result<LinkedAccount> {
    cancellation
        .run_until_cancelled(async {
            let pre = bind(component)?;
            let indices =
                storefront_provider::GuestIndices::new(&pre).map_err(|e| e.to_string())?;
            let mut invocation = Invocation::new(&pre).await?;
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
        })
        .await
        .ok_or_else(|| "account linking cancelled".to_owned())?
}

pub async fn authenticate(
    component: &CompiledPlugin,
    account_id: &str,
    credential: Option<&[u8]>,
) -> Result<Authentication> {
    let pre = bind(component)?;
    let indices = storefront_provider::GuestIndices::new(&pre).map_err(|e| e.to_string())?;
    let mut invocation = Invocation::new(&pre).await?;
    let guest = indices
        .load(&mut invocation.store, &invocation.instance)
        .map_err(|e| e.to_string())?;
    guest
        .call_authenticate(&mut invocation.store, account_id, credential)
        .await
        .map_err(|e| e.to_string())?
}

pub async fn list_games(
    component: &CompiledPlugin,
    account_id: &str,
    access: &[u8],
    cancellation: &CancellationToken,
) -> Result<Vec<OwnedGame>> {
    cancellation
        .run_until_cancelled(async {
            let pre = bind(component)?;
            let indices =
                storefront_provider::GuestIndices::new(&pre).map_err(|e| e.to_string())?;
            let mut invocation = Invocation::new(&pre).await?;
            let guest = indices
                .load(&mut invocation.store, &invocation.instance)
                .map_err(|e| e.to_string())?;
            guest
                .call_list_games(&mut invocation.store, account_id, access)
                .await
                .map_err(|e| e.to_string())?
        })
        .await
        .ok_or_else(|| "library enumeration cancelled".to_owned())?
}
