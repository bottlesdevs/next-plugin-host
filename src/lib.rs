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

pub use account::{Account, AccountIdentity, Interaction};
pub use library::{Library, LibraryEntry};
pub use manifest::Manifest;
pub use plugin::{Capability, Plugin};

use next_paths::plugins_dir;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};
use wasmtime::{Cache, CacheConfig, Engine, component::Linker};

use plugin::WasiState;

/// Owns the installed plugin packages and one loaded instance of each.
///
/// Packages live under the application's plugins directory at
/// `installed/<id>/plugin.toml` and `installed/<id>/plugin.wasm`, and each
/// plugin's private data at `data/<id>/`. Each installed plugin has at most
/// one loaded instance, shared by every handle and capability cast from it.
///
/// # Examples
///
/// ```text
/// let plugins = bottles_plugin_host::Plugins::new()?;
/// for (id, error) in plugins.reload().await? {
///     eprintln!("{id} failed to load: {error}");
/// }
/// for library in plugins.providing::<bottles_plugin_host::Library>() {
///     let entries = library.list_entries().await?;
/// }
/// ```
pub struct Plugins {
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
    /// until [`Plugins::reload`].
    ///
    /// # Errors
    ///
    /// Returns an error if the cache, engine, or host imports cannot be configured.
    pub fn new() -> wasmtime::Result<Self> {
        fs::create_dir_all(installed_dir())?;
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
        for entry in fs::read_dir(installed_dir())? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let manifest = Manifest::read(&entry.path())?;
                manifests.push(manifest);
            }
        }
        Ok(manifests)
    }

    /// Brings the loaded plugins in line with the installed packages.
    ///
    /// Loads every installed plugin that is not loaded and drops every loaded
    /// plugin that is no longer installed. This is the one path by which plugins
    /// are loaded: call it at startup and after [`Plugins::install`] or
    /// [`Plugins::uninstall`].
    ///
    /// A plugin that fails to load is left out, and its error is returned with
    /// its ID so the caller decides how to report it; the other plugins still
    /// load, and the next reload tries it again.
    ///
    /// # Errors
    ///
    /// Returns an error if the plugins directory cannot be read.
    pub async fn reload(&self) -> wasmtime::Result<Vec<(String, wasmtime::Error)>> {
        let mut installed = HashSet::new();
        for entry in fs::read_dir(installed_dir())? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                installed.insert(entry.file_name().to_string_lossy().into_owned());
            }
        }
        self.loaded
            .lock()
            .unwrap()
            .retain(|id, _| installed.contains(id));
        let mut failed = Vec::new();
        for id in installed {
            if self.get(&id).is_none()
                && let Err(error) = self.load(&id).await
            {
                failed.push((id, error));
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

    /// Copies a package from `source` into the plugins directory.
    ///
    /// Uses the source manifest's ID as the destination path, overwriting
    /// `plugin.toml` and `plugin.wasm`. A plugin already loaded under that ID is
    /// forgotten; handles to the old instance keep it alive until they are
    /// dropped. Call [`Plugins::reload`] to load the new version.
    ///
    /// # Errors
    ///
    /// Returns an error if the source manifest cannot be read or parsed, or
    /// either file cannot be copied. Files already copied are left in place if
    /// a later copy fails.
    pub fn install(&self, source: &Path) -> wasmtime::Result<Manifest> {
        let manifest = Manifest::read(source)?;
        let directory = package_dir(&manifest.id);
        fs::create_dir_all(&directory)?;
        fs::copy(manifest_path(source), manifest_path(&directory))?;
        fs::copy(component_path(source), component_path(&directory))?;
        self.loaded.lock().unwrap().remove(&manifest.id);
        Ok(manifest)
    }

    /// Forgets plugin `id` and removes its package and private data.
    ///
    /// Handles to its instance keep it alive until they are dropped.
    ///
    /// # Errors
    ///
    /// Returns the filesystem error if the package directory cannot be
    /// removed, including when it does not exist.
    pub fn uninstall(&self, id: &str) -> wasmtime::Result<()> {
        self.loaded.lock().unwrap().remove(id);
        fs::remove_dir_all(package_dir(id))?;
        if data_dir(id).exists() {
            fs::remove_dir_all(data_dir(id))?;
        }
        Ok(())
    }

    /// Loads installed plugin `id` on its own driver thread.
    async fn load(&self, id: &str) -> wasmtime::Result<Plugin> {
        let manifest = Manifest::read(&package_dir(id))?;
        let plugin = Plugin::load(manifest, self.linker.clone()).await?;
        self.loaded
            .lock()
            .unwrap()
            .insert(id.to_owned(), plugin.clone());
        Ok(plugin)
    }
}

/// The directory holding one package directory per installed plugin.
fn installed_dir() -> PathBuf {
    plugins_dir().join("installed")
}

/// The package directory of plugin `id`.
fn package_dir(id: &str) -> PathBuf {
    installed_dir().join(id)
}

/// The manifest file of the package in `package`.
fn manifest_path(package: &Path) -> PathBuf {
    package.join("plugin.toml")
}

/// The component file of the package in `package`.
fn component_path(package: &Path) -> PathBuf {
    package.join("plugin.wasm")
}

/// The private read-write directory of plugin `id`.
fn data_dir(id: &str) -> PathBuf {
    plugins_dir().join("data").join(id)
}
