use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::sync::Mutex;
use wasmtime::{
    Engine, Store,
    component::{Component, Instance, InstancePre, Linker, ResourceTable},
};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

use crate::{CompiledPlugin, PluginInfo, Result};

/// Shared compiler. Compilation executes no guest code.
pub(crate) struct Runtime {
    engine: Engine,
}

impl Runtime {
    pub(crate) fn new() -> Result<Self> {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        config.wasm_component_model_async(true);
        Ok(Self {
            engine: Engine::new(&config)?,
        })
    }

    pub(crate) async fn compile(&self, bytes: Vec<u8>) -> Result<Component> {
        let engine = self.engine.clone();
        blocking::unblock(move || Ok(Component::from_binary(&engine, &bytes)?)).await
    }
}

/// Adds standard WASI P3 and HTTP imports to a caller-owned linker.
pub fn add_to_linker<T: WasiView + WasiHttpView + 'static>(
    linker: &mut Linker<T>,
) -> wasmtime::Result<()> {
    wasmtime_wasi::p3::add_to_linker(linker)?;
    wasmtime_wasi_http::p3::add_to_linker(linker)
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

/// One caller-owned persistent instance and its typed bindings.
/// Clones share and serialize calls; separate openings retain independent guest state.
pub struct Plugin<State: 'static, Bindings> {
    compiled: Arc<CompiledPlugin>,
    instance: Arc<Mutex<Option<(PluginInstance<State>, Bindings)>>>,
}

// Deriving Clone would require State and Bindings to implement Clone, even though
// cloning this handle only clones the Arcs and shares the same instance.
impl<State: 'static, Bindings> Clone for Plugin<State, Bindings> {
    fn clone(&self) -> Self {
        Self {
            compiled: self.compiled.clone(),
            instance: self.instance.clone(),
        }
    }
}

impl<State: Send + 'static, Bindings: Send> Plugin<State, Bindings> {
    /// Takes ownership of the instance and bindings loaded from it.
    pub fn new(
        compiled: Arc<CompiledPlugin>,
        invocation: PluginInstance<State>,
        bindings: Bindings,
    ) -> Self {
        Self {
            compiled,
            instance: Arc::new(Mutex::new(Some((invocation, bindings)))),
        }
    }

    /// Borrows the compiled generation's metadata, including after this session closes.
    pub fn info(&self) -> &PluginInfo {
        &self.compiled.info
    }

    /// Drives one call on the caller's future, retaining guest state on success.
    ///
    /// Calls using WASI P3 imports must be polled in the caller's Tokio runtime
    /// with I/O and time enabled.
    ///
    /// Dropping an active call drops its store and bindings and closes this session.
    /// A runtime error also closes it; a WIT error returned inside `Ok` retains both.
    /// Dropping a call while it waits for the session leaves the running call intact.
    /// Closed sessions reject later calls; the caller must explicitly open a new one.
    pub async fn call<R, F>(&self, call: F) -> Result<R>
    where
        F: for<'a> FnOnce(
                &'a mut PluginInstance<State>,
                &'a mut Bindings,
            ) -> BoxFuture<'a, wasmtime::Result<R>>
            + Send,
    {
        let mut slot = self.instance.lock().await;
        let (mut invocation, mut bindings) = slot
            .take()
            .ok_or_else(|| wasmtime::Error::msg("plugin session is closed"))?;
        invocation.store.set_fuel(INVOCATION_FUEL)?;
        let result = call(&mut invocation, &mut bindings).await;
        if result.is_ok() {
            *slot = Some((invocation, bindings));
        }
        Ok(result?)
    }
}

/// A store and its instance, owned by the active call while a session is running.
pub struct PluginInstance<T: 'static> {
    /// Store holding the caller's state and guest memory.
    pub store: Store<T>,
    /// Component instance whose exports use this store.
    pub instance: Instance,
}

impl<T: Send + 'static> PluginInstance<T> {
    /// Instantiates a caller-linked component without spawning a task.
    /// When using WASI P3 imports, poll this future in the caller's Tokio runtime
    /// with I/O and time enabled.
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
