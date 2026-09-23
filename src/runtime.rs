use std::{future::Future, pin::Pin};

use futures::{StreamExt, channel::mpsc};
use tokio::sync::oneshot;
use wasmtime::{
    Engine, Store,
    component::{Component, Instance, InstancePre, Linker, ResourceTable},
};
use wasmtime_wasi::runtime::{AbortOnDropJoinHandle, spawn};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::{
    WasiHttpCtx,
    p2::{WasiHttpCtxView, WasiHttpView},
};

use crate::Result;

/// Shared compiler and the complete host import environment. Preparation executes no guest code.
pub(crate) struct Runtime {
    engine: Engine,
    linker: Linker<HostState>,
}

impl Runtime {
    pub(crate) fn new() -> Result<Self> {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
        crate::storefront::add_plugin_imports(&mut linker)?;
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

/// Resources owned by one loaded plugin runtime.
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
type CallFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type Call = Box<dyn for<'a> FnOnce(&'a mut Invocation) -> CallFuture<'a, bool> + Send>;

/// One serialized worker. Its owner retains the task; the task never retains its owner.
pub(crate) struct Worker {
    pub(super) sender: mpsc::UnboundedSender<Call>,
    _task: AbortOnDropJoinHandle<()>,
}

impl Worker {
    pub(crate) async fn new(pre: InstancePre<HostState>) -> Result<Self> {
        let mut invocation = spawn(Invocation::new(pre)).await?;
        let (sender, mut receiver) = mpsc::unbounded::<Call>();
        // ponytail: one queue serializes every capability; split only for a concrete concurrency need.
        let task = spawn(async move {
            while let Some(call) = receiver.next().await {
                if !call(&mut invocation).await {
                    break;
                }
            }
        });
        Ok(Self {
            sender,
            _task: task,
        })
    }

    pub(crate) async fn call<T, F>(&self, call: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a mut Invocation) -> CallFuture<'a, wasmtime::Result<T>>
            + Send
            + 'static,
    {
        let (reply, response) = oneshot::channel();
        self.sender
            .unbounded_send(Box::new(move |invocation| {
                Box::pin(async move {
                    let result = async {
                        invocation.store.set_fuel(INVOCATION_FUEL)?;
                        call(invocation).await
                    }
                    .await;
                    // WIT errors are values inside Ok; only runtime failures retire the Store.
                    let reusable = result.is_ok();
                    let _ = reply.send(result);
                    reusable
                })
            }))
            .map_err(|_| wasmtime::Error::msg("plugin worker stopped; reload the plugin"))?;
        Ok(response
            .await
            .map_err(|_| wasmtime::Error::msg("plugin worker stopped before replying"))??)
    }
}

/// The persistent Store and instance, accessed only by their worker.
pub(crate) struct Invocation {
    pub(crate) component: InstancePre<HostState>,
    pub(crate) store: Store<HostState>,
    pub(crate) instance: Instance,
}

impl Invocation {
    async fn new(pre: InstancePre<HostState>) -> Result<Self> {
        let state = HostState {
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new().inherit_stderr().build(),
            http: WasiHttpCtx::new(),
        };
        let mut store = Store::new(pre.engine(), state);
        store.set_fuel(INVOCATION_FUEL)?;
        store.fuel_async_yield_interval(Some(YIELD_INTERVAL))?;
        let instance = pre.instantiate_async(&mut store).await?;
        Ok(Self {
            component: pre,
            store,
            instance,
        })
    }
}
