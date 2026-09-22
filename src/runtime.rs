use wasmtime::{
    Engine, Store,
    component::{Component, Instance, InstancePre, Linker, ResourceTable},
};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::{
    WasiHttpCtx,
    p2::{WasiHttpCtxView, WasiHttpView},
};

use crate::Result;

/// Shared compiler and the complete host import environment. Preparation executes no guest code.
pub struct Runtime {
    engine: Engine,
    linker: Linker<HostState>,
}

impl Runtime {
    pub fn new(
        register_imports: impl FnOnce(&mut Linker<HostState>) -> wasmtime::Result<()>,
    ) -> Result<Self> {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
        register_imports(&mut linker)?;
        Ok(Self { engine, linker })
    }

    pub(crate) async fn compile(&self, bytes: Vec<u8>) -> Result<Component> {
        let engine = self.engine.clone();
        blocking::unblock(move || Ok(Component::from_binary(&engine, &bytes)?)).await
    }

    pub(crate) fn link(&self, component: &Component) -> Result<InstancePre<HostState>> {
        Ok(self.linker.instantiate_pre(component)?)
    }
}

/// Per-invocation host resources; adapters place their explicit capabilities in the table.
pub struct HostState {
    pub table: ResourceTable,
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

const INVOCATION_FUEL: u64 = 1_000_000_000;
const YIELD_INTERVAL: u64 = 100_000;
/// The Store and component instance owned by one independent call.
pub struct Invocation {
    pub store: Store<HostState>,
    pub instance: Instance,
}

impl Invocation {
    pub async fn new(pre: &InstancePre<HostState>) -> Result<Self> {
        let state = HostState {
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new().build(),
            http: WasiHttpCtx::new(),
        };
        let mut store = Store::new(pre.engine(), state);
        store.set_fuel(INVOCATION_FUEL)?;
        store.fuel_async_yield_interval(Some(YIELD_INTERVAL))?;
        let instance = pre.instantiate_async(&mut store).await?;
        Ok(Self { store, instance })
    }
}
