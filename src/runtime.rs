use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use wasmtime::{
    Engine, Store,
    component::{Component, HasSelf, Linker, Resource, ResourceAny, ResourceTable},
};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::{
    WasiHttpCtx,
    p2::{WasiHttpCtxView, WasiHttpView},
};

use crate::{
    AccountLinkInteraction, LinkedAccount, ListedGames, PluginKind, Result,
    bindings::{self, bottles::plugin::account_link},
};

/// One initialized WebAssembly component and its persistent plugin resource.
pub struct Plugin {
    provides: Vec<PluginKind>,
    instance: Mutex<PluginInstance>,
}

struct PluginInstance {
    store: Store<HostState>,
    guest: bindings::Plugin,
    resource: ResourceAny,
}

struct HostState {
    table: ResourceTable,
    wasi: WasiCtx,
    http: WasiHttpCtx,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for HostState {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: Default::default(),
        }
    }
}

impl account_link::HostInteraction for HostState {
    async fn request_input(
        &mut self,
        interaction: Resource<Arc<dyn AccountLinkInteraction>>,
        url: String,
        instructions: String,
    ) -> wasmtime::Result<std::result::Result<String, String>> {
        let interaction = self.table.get(&interaction)?.clone();
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

impl Plugin {
    /// Compiles and initializes one component with standard WASI and WASI HTTP.
    pub async fn load(component: &[u8]) -> Result<Self> {
        let engine = Engine::new(&wasmtime::Config::new()).map_err(|error| error.to_string())?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(|error| error.to_string())?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)
            .map_err(|error| error.to_string())?;
        account_link::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .map_err(|error| error.to_string())?;

        let component =
            Component::from_binary(&engine, component).map_err(|error| error.to_string())?;
        let state = HostState {
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new().build(),
            http: WasiHttpCtx::new(),
        };
        let mut store = Store::new(&engine, state);
        let guest = bindings::Plugin::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(|error| error.to_string())?;
        let provides = guest
            .bottles_plugin_lifecycle()
            .call_provides(&mut store)
            .await
            .map_err(|error| error.to_string())?;
        let resource = guest
            .bottles_plugin_lifecycle()
            .plugin()
            .call_new(&mut store)
            .await
            .map_err(|error| error.to_string())??;

        Ok(Self {
            provides,
            instance: Mutex::new(PluginInstance {
                store,
                guest,
                resource,
            }),
        })
    }

    /// The typed subsystem contributions declared by the component.
    pub fn provides(&self) -> &[PluginKind] {
        &self.provides
    }

    /// Invokes the storefront account-provider contribution.
    pub async fn link_account(
        &self,
        interaction: Arc<dyn AccountLinkInteraction>,
        cancellation: &CancellationToken,
    ) -> Result<LinkedAccount> {
        if !self
            .provides
            .contains(&PluginKind::StorefrontAccountProvider)
        {
            return Err("plugin does not advertise storefront-account-provider".into());
        }

        let Some(mut instance) = cancellation.run_until_cancelled(self.instance.lock()).await
        else {
            return Err("account linking cancelled".into());
        };
        let PluginInstance {
            store,
            guest,
            resource,
        } = &mut *instance;

        let interaction = store
            .data_mut()
            .table
            .push(interaction)
            .map_err(|error| error.to_string())?;
        let borrowed_interaction = Resource::new_borrow(interaction.rep());
        let result = cancellation
            .run_until_cancelled(
                guest
                    .bottles_plugin_storefront_account_provider()
                    .call_link_account(&mut *store, *resource, borrowed_interaction),
            )
            .await;
        store
            .data_mut()
            .table
            .delete(interaction)
            .map_err(|error| error.to_string())?;

        result
            .ok_or_else(|| "account linking cancelled".to_owned())?
            .map_err(|error| error.to_string())?
    }

    /// Invokes the storefront library-provider contribution.
    pub async fn list_games(
        &self,
        account_id: &str,
        credential: Option<&[u8]>,
    ) -> Result<ListedGames> {
        if !self
            .provides
            .contains(&PluginKind::StorefrontLibraryProvider)
        {
            return Err("plugin does not advertise storefront-library-provider".into());
        }

        let mut instance = self.instance.lock().await;
        let PluginInstance {
            store,
            guest,
            resource,
        } = &mut *instance;

        guest
            .bottles_plugin_storefront_library_provider()
            .call_list_games(&mut *store, *resource, account_id, credential)
            .await
            .map_err(|error| error.to_string())?
    }
}
