use bottles_core::{
    LibraryEntry, LibraryProvider, Operation, ProviderState,
    error::{Error, Result},
};

use futures::{StreamExt, stream::BoxStream};
use std::sync::Arc;
use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;
use wasmtime::component::{Accessor, Instance};

use crate::{
    Capability, Plugin, PluginInterface,
    plugin::{WasiState, call},
};

/// Listing state for plugins that export the library-provider interface.
///
/// [`Plugin<Library>`] implements [`bottles_core::LibraryProvider`], using the
/// manifest ID as its provider ID. Casting starts one listing on the driver's
/// thread; every entry watcher of that handle shares the published listing.
/// Subscriptions yield the current state immediately, starting with
/// [`ProviderState::Loading`] until the guest answers.
/// Launch returns a lazy [`Operation`] that queues its guest call when polled;
/// it does not emit progress updates.
///
/// The operation retains a call sender and can outlive the plugin handle.
/// Cancellation can end the wait with [`Error::Cancelled`], but does not
/// withdraw an already queued guest call. Binding, driver, and
/// guest failures become [`Error::LibraryProvider`].
///
/// # Examples
///
/// ```text
/// use bottles_core::{LibraryProvider, ProviderState};
/// use futures::StreamExt;
///
/// if let Some(library) = plugin.cast::<bottles_plugin_host::Library>() {
///     let mut states = library.entries();
///     while let Some(state) = states.next().await {
///         match &*state {
///             ProviderState::Loading => continue,
///             ProviderState::Loaded(entries) => {
///                 for entry in entries {
///                     println!("{}: {}", entry.id, entry.title);
///                 }
///             }
///             ProviderState::Failed(error) => return Err(error.to_string().into()),
///         }
///         break;
///     }
///     library.launch("entry-id")?.await?;
/// }
/// ```
pub struct Library {
    listing: watch::Sender<Arc<ProviderState>>,
}

impl Capability for Library {
    const INTERFACE: PluginInterface = PluginInterface::LibraryProvider;

    fn new(plugin: &Plugin) -> Self {
        let (listing, _) = watch::channel(Arc::new(ProviderState::Loading));
        let published = listing.clone();
        let provider_id = plugin.manifest().id.clone();
        let queued = plugin
            .shared
            .calls
            .send(Box::new(move |accessor, instance| {
                Box::pin(async move {
                    let state = match list_entries(accessor, instance, &provider_id).await {
                        Ok(entries) => ProviderState::Loaded(entries),
                        Err(error) => ProviderState::Failed(error),
                    };
                    published.send_replace(Arc::new(state));
                })
            }));
        if queued.is_err() {
            listing.send_replace(Arc::new(ProviderState::Failed(Error::LibraryProvider {
                provider: plugin.manifest().id.clone(),
                message: "plugin driver is closed".into(),
            })));
        }
        Self { listing }
    }
}

mod bindings {
    wasmtime::component::bindgen!({
        path: "../next-plugin-api/wit",
        world: "library",
        exports: { default: async | store },
    });
}

impl LibraryProvider for Plugin<Library> {
    fn id(&self) -> &str {
        &self.manifest().id
    }

    fn entries(&self) -> BoxStream<'static, Arc<ProviderState>> {
        WatchStream::new(self.capability.listing.subscribe()).boxed()
    }

    fn launch(&self, entry_id: &str) -> Result<Operation<()>> {
        let calls = self.shared.calls.clone();
        let provider_id = self.id().to_owned();
        let entry_id = entry_id.to_owned();
        Ok(Operation::new(move |_, cancellation| async move {
            cancellation
                .run_until_cancelled(call(&calls, move |accessor, instance| {
                    Box::pin(async move {
                        let bindings = accessor
                            .with(|mut access| bindings::Library::new(&mut access, instance))?;
                        bindings
                            .bottles_plugin_library_provider()
                            .call_launch(accessor, entry_id)
                            .await
                    })
                }))
                .await
                .ok_or(Error::Cancelled)?
                .map_err(|error| error.to_string())
                .and_then(|result| result)
                .map_err(|message| Error::LibraryProvider {
                    provider: provider_id,
                    message,
                })
        }))
    }
}

async fn list_entries(
    accessor: &Accessor<WasiState>,
    instance: &Instance,
    provider_id: &str,
) -> Result<Vec<LibraryEntry>> {
    let result = async {
        let bindings = accessor.with(|mut access| bindings::Library::new(&mut access, instance))?;
        bindings
            .bottles_plugin_library_provider()
            .call_list_entries(accessor)
            .await
    }
    .await;
    result
        .map_err(|error| error.to_string())
        .and_then(|result| result)
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| LibraryEntry {
                    id: entry.id,
                    title: entry.title,
                })
                .collect()
        })
        .map_err(|message| Error::LibraryProvider {
            provider: provider_id.to_owned(),
            message,
        })
}
