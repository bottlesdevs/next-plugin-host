#![doc = include_str!("../README.md")]
#![warn(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

/// Links accounts through account exports and caller-supplied input interactions.
mod account;
/// Lists and launches entries through library exports.
mod library;
/// Reads the metadata stored beside a component.
mod manifest;
/// Owns loaded instances, call dispatch, and typed capability handles.
mod plugin;

mod interfaces {
    include!(concat!(env!("OUT_DIR"), "/plugin_interfaces.rs"));
}

pub use account::{Account, AccountIdentity, Interaction, LinkedAccount};
pub use library::{Library, LibraryEntry};
pub use manifest::Manifest;
pub use plugin::{Capability, Plugin};

use next_paths::plugins_dir;
use std::{collections::HashMap, fs, path::Path, sync::Mutex};
use wasmtime::{Cache, CacheConfig, Engine, component::Linker};

use plugin::WasiState;

/// Owns the installed plugin packages and one loaded instance of each.
///
/// Packages live under the application's plugins directory at
/// `<id>/plugin.toml` and `<id>/plugin.wasm`. Each installed plugin has at most
/// one loaded instance, shared by every handle and capability cast from it.
///
/// # Examples
///
/// ```text
/// let plugins = bottles_plugin_host::Plugins::new()?;
/// for (id, error) in plugins.load_installed().await? {
///     eprintln!("{id} failed to load: {error}");
/// }
/// for library in plugins.providing::<bottles_plugin_host::Library>() {
///     let entries = library.list_entries().await?;
/// }
/// ```
pub struct Plugins {
    engine: Engine,
    linker: Linker<WasiState>,
    /// Loaded plugins, by ID.
    loaded: Mutex<HashMap<String, Plugin>>,
}

impl Plugins {
    /// Creates the shared engine, linker, and persistent compilation cache.
    ///
    /// Adds WASIp3, outbound HTTP, and account input imports. Packages are loaded
    /// from the application's plugins directory, which is created if missing.
    /// Compiled components are cached across launches in the `wasmtime`
    /// subdirectory of the application's cache directory. No plugin is loaded
    /// until [`Plugins::load_installed`] or [`Plugins::install`].
    ///
    /// # Errors
    ///
    /// Returns an error if the cache, engine, or host imports cannot be configured.
    pub fn new() -> wasmtime::Result<Self> {
        fs::create_dir_all(plugins_dir())?;
        let mut cache = CacheConfig::new();
        cache.with_directory(next_paths::cache_dir().join("wasmtime"));
        let mut config = wasmtime::Config::new();
        config.wasm_component_model_async(true);
        config.cache(Some(Cache::new(cache)?));
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p3::add_to_linker(&mut linker)?;
        wasmtime_wasi_http::p3::add_to_linker(&mut linker)?;
        account::add_to_linker(&mut linker)?;
        Ok(Self {
            engine,
            linker,
            loaded: Mutex::new(HashMap::new()),
        })
    }

    /// Reads the manifests of installed packages.
    ///
    /// Reads `plugin.toml` from every immediate child directory. Other entries
    /// are skipped. Results follow filesystem order and are not cached.
    ///
    /// # Errors
    ///
    /// Returns an error if the plugins directory cannot be read, an entry's type
    /// cannot be determined, or any directory's manifest cannot be read or parsed.
    pub fn list(&self) -> wasmtime::Result<Vec<Manifest>> {
        let mut manifests = Vec::new();
        for entry in fs::read_dir(plugins_dir())? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let manifest = Manifest::read(&entry.path().join("plugin.toml"))?;
                manifests.push(manifest);
            }
        }
        Ok(manifests)
    }

    /// Loads every installed plugin that is not loaded yet.
    ///
    /// A plugin that fails to load is left out, and its error is returned with
    /// its ID so the caller decides how to report it; the other plugins still load.
    ///
    /// # Errors
    ///
    /// Returns an error if the installed packages cannot be listed.
    pub async fn load_installed(&self) -> wasmtime::Result<Vec<(String, wasmtime::Error)>> {
        let mut failed = Vec::new();
        for manifest in self.list()? {
            if self.get(&manifest.id).is_some() {
                continue;
            }
            if let Err(error) = self.load(&manifest.id).await {
                failed.push((manifest.id, error));
            }
        }
        Ok(failed)
    }

    /// Returns the loaded plugin with ID `id`.
    pub fn get(&self, id: &str) -> Option<Plugin> {
        self.loaded.lock().unwrap().get(id).cloned()
    }

    /// Returns every loaded plugin that provides capability `C`.
    ///
    /// # Examples
    ///
    /// ```text
    /// for library in plugins.providing::<bottles_plugin_host::Library>() {
    ///     println!("{}", library.manifest().name);
    /// }
    /// ```
    pub fn providing<C: Capability>(&self) -> Vec<Plugin<C>> {
        self.loaded
            .lock()
            .unwrap()
            .values()
            .filter_map(Plugin::cast)
            .collect()
    }

    /// Installs a package from `source`, then loads it.
    ///
    /// Uses the source manifest's ID as the destination path, overwriting
    /// `plugin.toml` and `plugin.wasm`. A plugin already loaded under that ID is
    /// replaced; handles to the old instance keep it alive until they are dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if the source manifest cannot be read or parsed, either
    /// file cannot be copied, or the installed plugin cannot be loaded. Files
    /// already copied are left in place if a later step fails.
    pub async fn install(&self, source: &Path) -> wasmtime::Result<Manifest> {
        let manifest = Manifest::read(&source.join("plugin.toml"))?;
        let directory = plugins_dir().join(&manifest.id);
        fs::create_dir_all(&directory)?;
        fs::copy(source.join("plugin.toml"), directory.join("plugin.toml"))?;
        fs::copy(source.join("plugin.wasm"), directory.join("plugin.wasm"))?;
        self.load(&manifest.id).await?;
        Ok(manifest)
    }

    /// Unloads plugin `id` and removes its package.
    ///
    /// Handles to the unloaded instance keep it alive until they are dropped.
    ///
    /// # Errors
    ///
    /// Returns the filesystem error if the package directory cannot be
    /// removed, including when it does not exist.
    pub fn uninstall(&self, id: &str) -> wasmtime::Result<()> {
        self.loaded.lock().unwrap().remove(id);
        Ok(fs::remove_dir_all(plugins_dir().join(id))?)
    }

    /// Loads installed plugin `id` on its own driver thread, replacing any
    /// instance already loaded under that ID.
    async fn load(&self, id: &str) -> wasmtime::Result<Plugin> {
        let directory = plugins_dir().join(id);
        let manifest = Manifest::read(&directory.join("plugin.toml"))?;
        let plugin = Plugin::load(
            manifest,
            directory.join("plugin.wasm"),
            self.engine.clone(),
            self.linker.clone(),
        )
        .await?;
        self.loaded
            .lock()
            .unwrap()
            .insert(id.to_owned(), plugin.clone());
        Ok(plugin)
    }
}
