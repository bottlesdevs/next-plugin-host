use futures::future::BoxFuture;
use tokio::sync::Mutex;
use wasmtime::{
    Engine, Store,
    component::{Component, Instance, InstancePre, Linker, ResourceTable},
};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

use crate::Result;

/// Shared compiler. Compilation executes no guest code.
pub(crate) struct Runtime {
    engine: Engine,
}

impl Runtime {
    pub(crate) fn new() -> Result<Self> {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        Ok(Self {
            engine: Engine::new(&config)?,
        })
    }

    pub(crate) async fn compile(&self, bytes: Vec<u8>) -> Result<Component> {
        let engine = self.engine.clone();
        blocking::unblock(move || Ok(Component::from_binary(&engine, &bytes)?)).await
    }
}

/// Adds standard WASI and HTTP imports to a caller-owned linker.
pub fn add_to_linker<T: WasiView + WasiHttpView + 'static>(
    linker: &mut Linker<T>,
) -> wasmtime::Result<()> {
    wasmtime_wasi::p2::add_to_linker_async(linker)?;
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(linker)
}

/// Standard WASI state shared with the caller's domain imports.
pub struct WasiState {
    /// Resource table shared by all interfaces in this store.
    pub table: ResourceTable,
    /// Filesystem, environment, and other standard WASI capabilities.
    pub wasi: WasiCtx,
    /// Standard HTTP context.
    pub http: WasiHttpCtx,
}

impl WasiState {
    /// Uses the caller's WASI capabilities with a fresh resource table and HTTP context.
    pub fn new(wasi: WasiCtx) -> Self {
        Self {
            table: ResourceTable::new(),
            wasi,
            http: WasiHttpCtx::new(),
        }
    }
}

impl WasiView for WasiState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for WasiState {
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

/// One caller-owned persistent instance. Calls on this session are serialized.
/// Separate sessions retain independent guest state and run independently.
pub struct Session<T: 'static> {
    invocation: Mutex<Option<Invocation<T>>>,
}

impl<T: Send + 'static> Session<T> {
    /// Takes ownership after the caller has loaded any typed export handles.
    pub fn new(invocation: Invocation<T>) -> Self {
        Self {
            invocation: Mutex::new(Some(invocation)),
        }
    }

    /// Drives one call on the caller's future, retaining guest state on success.
    ///
    /// Dropping an active call drops its store and closes this session. A runtime
    /// error also closes it; a WIT error returned inside `Ok` retains the instance.
    /// Dropping a call while it waits for the session leaves the running call intact.
    /// Closed sessions reject later calls; the caller must explicitly open a new one.
    pub async fn call<R, F>(&self, call: F) -> Result<R>
    where
        F: for<'a> FnOnce(&'a mut Invocation<T>) -> BoxFuture<'a, wasmtime::Result<R>> + Send,
    {
        let mut slot = self.invocation.lock().await;
        let mut invocation = slot
            .take()
            .ok_or_else(|| wasmtime::Error::msg("plugin session is closed"))?;
        invocation.store.set_fuel(INVOCATION_FUEL)?;
        let result = call(&mut invocation).await;
        if result.is_ok() {
            *slot = Some(invocation);
        }
        Ok(result?)
    }
}

/// A store and its instance, owned by the active call while a session is running.
pub struct Invocation<T: 'static> {
    /// Store holding the caller's state and guest memory.
    pub store: Store<T>,
    /// Component instance whose exports use this store.
    pub instance: Instance,
}

impl<T: Send + 'static> Invocation<T> {
    /// Instantiates a caller-linked component without spawning a task.
    pub async fn new(pre: &InstancePre<T>, state: T) -> Result<Self> {
        let mut store = Store::new(pre.engine(), state);
        store.set_fuel(INVOCATION_FUEL)?;
        store.fuel_async_yield_interval(Some(YIELD_INTERVAL))?;
        let instance = pre.instantiate_async(&mut store).await?;
        Ok(Self { store, instance })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_initializes_with_selected_wasmtime_features() {
        super::Runtime::new().expect("Wasmtime engine must initialize");
    }
}
