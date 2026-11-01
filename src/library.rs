use async_trait::async_trait;
use bottles_core::{
    LibraryEntry, LibraryProvider, Operation,
    error::{Error, Result},
};

use crate::{Capability, Plugin, PluginInterface, plugin::call};

/// Marker for plugins that export the library-provider interface.
///
/// [`Plugin<Library>`] implements [`bottles_core::LibraryProvider`], using the
/// manifest ID as its provider ID. Listing queries the guest's current state.
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
/// use bottles_core::LibraryProvider;
///
/// if let Some(library) = plugin.cast::<bottles_plugin_host::Library>() {
///     for entry in library.list_entries().await? {
///         println!("{}: {}", entry.id, entry.title);
///     }
///     library.launch("entry-id")?.await?;
/// }
/// ```
pub struct Library;

impl Capability for Library {
    const INTERFACE: PluginInterface = PluginInterface::LibraryProvider;
}

mod bindings {
    wasmtime::component::bindgen!({
        path: "../next-plugin-api/wit",
        world: "library",
        exports: { default: async | store },
    });
}

#[async_trait]
impl LibraryProvider for Plugin<Library> {
    fn id(&self) -> &str {
        &self.manifest().id
    }

    async fn list_entries(&self) -> Result<Vec<LibraryEntry>> {
        call(&self.shared.calls, |accessor, instance| {
            Box::pin(async move {
                let bindings =
                    accessor.with(|mut access| bindings::Library::new(&mut access, instance))?;
                bindings
                    .bottles_plugin_library_provider()
                    .call_list_entries(accessor)
                    .await
            })
        })
        .await
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
            provider: self.id().to_owned(),
            message,
        })
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
