use crate::{Capability, Plugin, PluginInterface, plugin::call};

/// Launchable entry returned by a library plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryEntry {
    /// Identifier understood by the guest's launch method.
    pub id: String,
    /// Human-readable title.
    pub title: String,
}

/// Marker for plugins exporting the library-provider interface.
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

impl Plugin<Library> {
    /// Queries the guest's current library.
    pub async fn list_entries(&self) -> Result<Vec<LibraryEntry>, String> {
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
    }

    /// Queues a guest launch when this future is polled.
    pub async fn launch(&self, entry_id: &str) -> Result<(), String> {
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
        .await
        .map_err(|error| error.to_string())?
    }
}
