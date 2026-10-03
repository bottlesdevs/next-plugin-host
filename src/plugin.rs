use std::{marker::PhantomData, path::PathBuf, sync::Arc};

use futures::{
    future::BoxFuture,
    stream::{FuturesUnordered, StreamExt},
};
use tokio::sync::{mpsc, oneshot};
use wasmtime::{
    Engine, Store,
    component::{Accessor, Component, Instance, Linker, ResourceTable},
};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

use crate::{Manifest, PluginInterface};

pub(crate) struct WasiState {
    pub(crate) table: ResourceTable,
    wasi: WasiCtx,
    http: WasiHttpCtx,
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

type Call =
    Box<dyn for<'a> FnOnce(&'a Accessor<WasiState>, &'a Instance) -> BoxFuture<'a, ()> + Send>;

pub(crate) struct Shared {
    manifest: Manifest,
    component: Component,
    pub(crate) calls: mpsc::UnboundedSender<Call>,
}

/// A loaded plugin with one instance shared by its exported capabilities.
pub struct Plugin<C = ()> {
    pub(crate) shared: Arc<Shared>,
    capability: PhantomData<fn() -> C>,
}

/// Links a capability to the exported interface it requires.
pub trait Capability {
    const INTERFACE: PluginInterface;
}

impl<C> Plugin<C> {
    /// Returns the installed package metadata.
    pub fn manifest(&self) -> &Manifest {
        &self.shared.manifest
    }
}

impl Plugin {
    /// Reports whether this component exports the named interface.
    fn exports(&self, interface: PluginInterface) -> bool {
        self.shared
            .component
            .get_export_index(None, interface.as_str())
            .is_some()
    }

    /// Returns a shared handle when this component exports the capability's interface.
    pub fn cast<C: Capability>(&self) -> Option<Plugin<C>> {
        self.exports(C::INTERFACE).then(|| Plugin {
            shared: self.shared.clone(),
            capability: PhantomData,
        })
    }

    pub(crate) async fn load(
        manifest: Manifest,
        path: PathBuf,
        engine: Engine,
        linker: Linker<WasiState>,
    ) -> wasmtime::Result<Self> {
        let (calls, incoming) = mpsc::unbounded_channel::<Call>();
        let (ready_send, ready_receive) = oneshot::channel();
        std::thread::Builder::new()
            .name(format!("plugin-{}", manifest.id))
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => {
                        runtime.block_on(drive(engine, linker, path, ready_send, incoming))
                    }
                    Err(error) => {
                        let _ = ready_send.send(Err(error.into()));
                    }
                }
            })?;
        let component = ready_receive
            .await
            .map_err(|_| wasmtime::Error::msg("plugin driver stopped before initialization"))??;
        Ok(Self {
            shared: Arc::new(Shared {
                manifest,
                component,
                calls,
            }),
            capability: PhantomData,
        })
    }
}

async fn drive(
    engine: Engine,
    linker: Linker<WasiState>,
    path: PathBuf,
    ready_send: oneshot::Sender<wasmtime::Result<Component>>,
    mut incoming: mpsc::UnboundedReceiver<Call>,
) {
    let result = async {
        let component = Component::from_file(&engine, path)?;
        let pre = linker.instantiate_pre(&component)?;
        let mut store = Store::new(
            &engine,
            WasiState {
                table: ResourceTable::new(),
                wasi: WasiCtxBuilder::new().inherit_stderr().build(),
                http: WasiHttpCtx::new(),
            },
        );
        let instance = pre.instantiate_async(&mut store).await?;
        Ok::<_, wasmtime::Error>((store, instance, component))
    }
    .await;
    let (mut store, instance, component) = match result {
        Ok(value) => value,
        Err(error) => {
            let _ = ready_send.send(Err(error));
            return;
        }
    };
    let _ = ready_send.send(Ok(component));
    let _ = store
        .run_concurrent(async |accessor| {
            let mut active = FuturesUnordered::new();
            loop {
                tokio::select! {
                    call = incoming.recv() => match call {
                        Some(call) => active.push(call(accessor, &instance)),
                        None => break,
                    },
                    Some(()) = active.next(), if !active.is_empty() => {},
                }
            }
        })
        .await;
}

pub(crate) async fn call<R: Send + 'static>(
    calls: &mpsc::UnboundedSender<Call>,
    f: impl for<'a> FnOnce(&'a Accessor<WasiState>, &'a Instance) -> BoxFuture<'a, wasmtime::Result<R>>
    + Send
    + 'static,
) -> wasmtime::Result<R> {
    let (send, receive) = oneshot::channel();
    calls
        .send(Box::new(move |accessor, instance| {
            Box::pin(async move {
                let _ = send.send(f(accessor, instance).await);
            })
        }))
        .map_err(|_| wasmtime::Error::msg("plugin driver is closed"))?;
    receive
        .await
        .map_err(|_| wasmtime::Error::msg("plugin driver is closed"))?
}
