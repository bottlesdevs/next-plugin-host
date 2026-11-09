use std::{fs, marker::PhantomData, sync::Arc};

use futures::{
    future::BoxFuture,
    stream::{FuturesUnordered, StreamExt},
};
use tokio::sync::{mpsc, oneshot};
use wasmtime::{
    Store,
    component::{Accessor, Component, Instance as WasmInstance, Linker, ResourceTable},
};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

use crate::{Manifest, component_path, data_dir, package_dir};

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
    Box<dyn for<'a> FnOnce(&'a Accessor<WasiState>, &'a WasmInstance) -> BoxFuture<'a, ()> + Send>;

/// A running plugin, shared by its capability handles.
pub(crate) struct Instance {
    manifest: Manifest,
    component: Component,
    calls: mpsc::UnboundedSender<Call>,
}

/// A handle to a loaded plugin exposing capability `C`'s calls.
///
/// Obtained from [`crate::Plugins::get`] or [`crate::Plugins::providing`]. Every
/// handle to a plugin shares its store and guest state.
///
/// Handles and pending calls retain the driver's call channel.
/// The driver exits when it observes that all senders have been dropped.
///
/// # Examples
///
/// ```text
/// let account = plugins.get::<bottles_plugin_host::Account>("example");
/// let library = plugins.get::<bottles_plugin_host::Library>("example");
/// ```
pub struct Plugin<C> {
    shared: Arc<Instance>,
    capability: PhantomData<fn() -> C>,
}

// Written by hand because `#[derive(Clone)]` would require `C: Clone`.
impl<C> Clone for Plugin<C> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            capability: PhantomData,
        }
    }
}

/// Links a capability to the exported interface it requires.
///
/// [`crate::Account`] and [`crate::Library`] are the built-in markers. This
/// mapping controls export discovery; capability calls are implemented
/// separately on their corresponding typed [`Plugin`] handles.
///
/// # Examples
///
/// ```text
/// use bottles_plugin_host::{Capability, Library};
///
/// let name = <Library as Capability>::INTERFACE; // "bottles:plugin/library-provider@0.1.0"
/// ```
pub trait Capability {
    /// Names the versioned WIT interface a plugin must export to provide this capability.
    const INTERFACE: &'static str;
}

impl<C> Plugin<C> {
    /// Returns the installed package metadata.
    ///
    /// This is the manifest captured during loading. Changes to package files
    /// do not update the loaded handle's metadata.
    ///
    /// # Examples
    ///
    /// ```text
    /// println!("{} {}", plugin.manifest().name, plugin.manifest().version);
    /// ```
    pub fn manifest(&self) -> &Manifest {
        &self.shared.manifest
    }

    /// Queues `f` on the plugin's driver and waits for its result.
    pub(crate) async fn call<R: Send + 'static>(
        &self,
        f: impl for<'a> FnOnce(
            &'a Accessor<WasiState>,
            &'a WasmInstance,
        ) -> BoxFuture<'a, wasmtime::Result<R>>
        + Send
        + 'static,
    ) -> wasmtime::Result<R> {
        let (reply, response) = oneshot::channel();
        self.shared
            .calls
            .send(Box::new(move |accessor, instance| {
                Box::pin(async move {
                    let _ = reply.send(f(accessor, instance).await);
                })
            }))
            .map_err(|_| wasmtime::Error::msg("plugin driver is closed"))?;
        response
            .await
            .map_err(|_| wasmtime::Error::msg("plugin driver is closed"))?
    }
}

impl Instance {
    /// Returns a handle for capability `C` when this component exports its interface.
    ///
    /// Returns [`None`] when the exact versioned name in [`Capability::INTERFACE`]
    /// is absent. This checks only that name, not function signatures. The
    /// adapter binds its world on each invocation, where incompatibilities are
    /// returned as call errors.
    pub(crate) fn handle<C: Capability>(self: &Arc<Self>) -> Option<Plugin<C>> {
        self.component
            .get_export_index(None, C::INTERFACE)
            .is_some()
            .then(|| Plugin {
                shared: self.clone(),
                capability: PhantomData,
            })
    }

    /// Starts the plugin on its own driver thread.
    pub(crate) async fn load(
        manifest: Manifest,
        linker: Linker<WasiState>,
    ) -> wasmtime::Result<Arc<Self>> {
        let id = manifest.id.clone();
        let (calls, incoming) = mpsc::unbounded_channel::<Call>();
        let (ready, started) = oneshot::channel();
        std::thread::Builder::new()
            .name(format!("plugin-{}", manifest.id))
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(drive(linker, id, ready, incoming)),
                    Err(error) => {
                        let _ = ready.send(Err(error.into()));
                    }
                }
            })?;
        let component = started
            .await
            .map_err(|_| wasmtime::Error::msg("plugin driver stopped before initialization"))??;
        Ok(Arc::new(Self {
            manifest,
            component,
            calls,
        }))
    }
}

async fn drive(
    linker: Linker<WasiState>,
    id: String,
    ready: oneshot::Sender<wasmtime::Result<Component>>,
    mut incoming: mpsc::UnboundedReceiver<Call>,
) {
    let result = async {
        let engine = linker.engine();
        let component = Component::from_file(engine, component_path(&package_dir(&id)))?;
        let pre = linker.instantiate_pre(&component)?;
        fs::create_dir_all(data_dir(&id))?;
        let mut wasi = WasiCtxBuilder::new();
        wasi.inherit_stderr()
            .preopened_dir(data_dir(&id), ".", FsPerms::ReadWrite)?;
        let mut store = Store::new(
            engine,
            WasiState {
                table: ResourceTable::new(),
                wasi: wasi.build(),
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
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(component));
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
