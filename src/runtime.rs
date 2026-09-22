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

/// Shared compiler. Compiling a component does not instantiate or execute it.
pub struct Runtime {
    engine: Engine,
}

impl Runtime {
    pub fn new() -> Result<Self> {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        Ok(Self {
            engine: Engine::new(&config).map_err(|e| e.to_string())?,
        })
    }

    pub async fn compile(&self, bytes: Vec<u8>) -> Result<CompiledPlugin> {
        let engine = self.engine.clone();
        blocking::unblock(move || {
            Component::from_binary(&engine, &bytes)
                .map(|component| CompiledPlugin { component })
                .map_err(|e| e.to_string())
        })
        .await
    }
}

/// Immutable code shared by independent invocations and contribution adapters.
pub struct CompiledPlugin {
    pub(crate) component: Component,
}

pub(crate) struct HostState {
    pub(crate) table: ResourceTable,
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
pub(crate) struct Invocation {
    pub(crate) store: Store<HostState>,
    pub(crate) instance: Instance,
}

impl Invocation {
    pub(crate) async fn new(pre: &InstancePre<HostState>) -> Result<Self> {
        let state = HostState {
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new().build(),
            http: WasiHttpCtx::new(),
        };
        let mut store = Store::new(pre.engine(), state);
        store.set_fuel(INVOCATION_FUEL).map_err(|e| e.to_string())?;
        store
            .fuel_async_yield_interval(Some(YIELD_INTERVAL))
            .map_err(|e| e.to_string())?;
        let instance = pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self { store, instance })
    }
}

impl CompiledPlugin {
    pub(crate) fn linker(&self) -> Result<Linker<HostState>> {
        let mut linker = Linker::new(self.component.engine());
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(|e| e.to_string())?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)
            .map_err(|e| e.to_string())?;
        Ok(linker)
    }
}
