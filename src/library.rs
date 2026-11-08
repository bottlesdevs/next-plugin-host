use crate::{Capability, Plugin, interfaces, plugin::call};

/// Marker for plugins that export the library-provider interface.
///
/// [`Plugin<Library>`] lists the guest's current entries and forwards launch
/// requests. Each call is queued to the plugin's driver; dropping the returned
/// future stops waiting but does not withdraw an already queued guest call.
/// Binding, driver, and guest failures are returned as errors.
///
/// # Examples
///
/// ```text
/// if let Some(library) = plugin.cast::<bottles_plugin_host::Library>() {
///     for entry in library.list_entries().await? {
///         println!("{}: {}", entry.id, entry.title);
///     }
///     library.launch("entry-id").await?;
/// }
/// ```
pub struct Library;

impl Capability for Library {
    const INTERFACE: &'static str = interfaces::LIBRARY_PROVIDER;
}

#[allow(missing_docs)]
mod bindings {
    wasmtime::component::bindgen!({
        path: "../next-plugin-api/wit",
        world: "library",
        exports: { default: async | store },
    });
}

/// One installed title reported by a library plugin.
///
/// `id` is local to the plugin and is passed back unchanged to
/// [`Plugin::<Library>::launch`].
pub use bindings::exports::bottles::plugin::library_provider::LibraryEntry;

impl Plugin<Library> {
    /// Asks the guest for its current installed, launchable titles.
    ///
    /// # Errors
    ///
    /// Returns an error if the driver is closed, the bindings cannot be
    /// constructed, or the guest reports a failure.
    pub async fn list_entries(&self) -> wasmtime::Result<Vec<LibraryEntry>> {
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
        .await?
        .map_err(wasmtime::Error::msg)
    }

    /// Asks the guest to launch an entry returned by [`Self::list_entries`].
    ///
    /// Completion means the launch request finished, not that the title exited.
    ///
    /// # Errors
    ///
    /// Returns an error if the driver is closed, the bindings cannot be
    /// constructed, or the guest reports a failure.
    pub async fn launch(&self, entry_id: &str) -> wasmtime::Result<()> {
        let entry_id = entry_id.to_owned();
        call(&self.shared.calls, move |accessor, instance| {
            Box::pin(async move {
                let bindings =
                    accessor.with(|mut access| bindings::Library::new(&mut access, instance))?;
                bindings
                    .bottles_plugin_library_provider()
                    .call_launch(accessor, entry_id)
                    .await
            })
        })
        .await?
        .map_err(wasmtime::Error::msg)
    }
}
