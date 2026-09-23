//! Typed calls for installed, launchable plugin entries.

use crate::LoadedPlugin;

mod bindings {
    wasmtime::component::bindgen!({
        path: "../next-plugin-api/wit",
        world: "library",
        additional_derives: [serde::Serialize, serde::Deserialize, PartialEq, Eq],
        exports: { default: async },
    });
}

use bindings::exports::bottles::plugin::library_provider;
pub use library_provider::LibraryEntry;
type Result<T> = std::result::Result<T, String>;

/// Enumerates entries using the plugin's current state.
pub async fn list_entries(plugin: &LoadedPlugin) -> Result<Vec<LibraryEntry>> {
    plugin
        .worker
        .call(move |invocation| {
            Box::pin(async move {
                let guest = match library_provider::GuestIndices::new(&invocation.component)
                    .and_then(|indices| indices.load(&mut invocation.store, &invocation.instance))
                {
                    Ok(guest) => guest,
                    Err(error) => return Ok(Err(error.to_string())),
                };
                guest.call_list_entries(&mut invocation.store).await
            })
        })
        .await
        .map_err(|error| error.to_string())?
}

/// Awaits the launch request, not the lifetime of the launched title.
/// Dropping this future does not cancel an invocation already accepted by the worker.
pub async fn launch(plugin: &LoadedPlugin, entry_id: &str) -> Result<()> {
    let entry_id = entry_id.to_owned();
    plugin
        .worker
        .call(move |invocation| {
            Box::pin(async move {
                let guest = match library_provider::GuestIndices::new(&invocation.component)
                    .and_then(|indices| indices.load(&mut invocation.store, &invocation.instance))
                {
                    Ok(guest) => guest,
                    Err(error) => return Ok(Err(error.to_string())),
                };
                guest.call_launch(&mut invocation.store, &entry_id).await
            })
        })
        .await
        .map_err(|error| error.to_string())?
}
