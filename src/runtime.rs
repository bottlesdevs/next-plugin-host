use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::sync::Mutex;
use wasmtime::{
    Engine, Store,
    component::{Accessor, Component, Instance, InstancePre, Linker, ResourceTable},
};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

use crate::{PluginInfo, Result, packages::CompiledPlugin};

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
pub(crate) fn add_to_linker(linker: &mut Linker<WasiState>) -> wasmtime::Result<()> {
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

/// Typed bindings to a plugin's shared guest session.
/// Clones share the bindings; all bindings for one plugin serialize calls through its store.
pub struct Plugin<Bindings> {
    session: Arc<PluginSession>,
    bindings: Arc<Bindings>,
}

// Deriving Clone would require Bindings to implement Clone, even though
// cloning this handle only clones the Arcs and shares the same instance.
impl<Bindings> Clone for Plugin<Bindings> {
    fn clone(&self) -> Self {
        Self {
            session: self.session.clone(),
            bindings: self.bindings.clone(),
        }
    }
}

impl<Bindings: Send + Sync> Plugin<Bindings> {
    /// Attaches typed bindings to a shared session.
    pub(crate) fn new(session: Arc<PluginSession>, bindings: Bindings) -> Self {
        Self {
            session,
            bindings: Arc::new(bindings),
        }
    }

    /// Borrows the compiled generation's metadata, including after this session closes.
    pub fn info(&self) -> &PluginInfo {
        &self.session.compiled.info
    }

    /// Drives one call on the caller's future, retaining guest state on success.
    /// The callback receives this session's concurrent accessor and typed bindings.
    ///
    /// Calls using WASI P3 imports must be polled in the caller's Tokio runtime
    /// with I/O and time enabled.
    ///
    /// Dropping an active call drops its store and closes this session.
    /// A runtime error also closes it; a WIT error returned inside `Ok` retains it.
    /// Dropping a call while it waits for the session leaves the running call intact.
    /// Closed sessions reject later calls; a later catalog load opens a new session.
    pub async fn call<R, F>(&self, call: F) -> Result<R>
    where
        F: for<'a> FnOnce(
                &'a Accessor<WasiState>,
                &'a Bindings,
            ) -> BoxFuture<'a, wasmtime::Result<R>>
            + Send,
    {
        let mut slot = self.session.store.lock().await;
        let mut store = slot
            .take()
            .ok_or_else(|| wasmtime::Error::msg("plugin session is closed"))?;
        store.set_fuel(INVOCATION_FUEL)?;
        let result = store
            .run_concurrent(async |accessor| call(accessor, &self.bindings).await)
            .await?;
        if result.is_ok() {
            *slot = Some(store);
        }
        Ok(result?)
    }
}

/// One shared guest session for all typed interfaces of a plugin generation.
pub(crate) struct PluginSession {
    pub(crate) compiled: Arc<CompiledPlugin>,
    /// Component instance whose exports use this store.
    pub(crate) instance: Instance,
    /// Store holding WASI state and guest memory; absent after closure.
    pub(crate) store: Mutex<Option<Store<WasiState>>>,
}

impl PluginSession {
    /// Instantiates a caller-linked component without spawning a task.
    /// When using WASI P3 imports, poll this future in the caller's Tokio runtime
    /// with I/O and time enabled.
    pub(crate) async fn new(
        compiled: Arc<CompiledPlugin>,
        pre: &InstancePre<WasiState>,
        state: WasiState,
    ) -> Result<Self> {
        let mut store = Store::new(pre.engine(), state);
        store.set_fuel(INVOCATION_FUEL)?;
        store.fuel_async_yield_interval(Some(YIELD_INTERVAL))?;
        let instance = pre.instantiate_async(&mut store).await?;
        Ok(Self {
            compiled,
            instance,
            store: Mutex::new(Some(store)),
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_initializes_with_selected_wasmtime_features() {
        super::Runtime::new().expect("Wasmtime engine must initialize");
    }
}
