#![doc = include_str!("../README.md")]
#![warn(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

/// Adapts account exports and host input interactions to core's account provider.
mod account;
/// Adapts library exports to core's library provider and lazy launch operations.
mod library;
/// Reads the metadata stored beside a component.
mod manifest;
/// Owns loaded instances, call dispatch, and typed capability handles.
mod plugin;

mod interfaces {
    include!(concat!(env!("OUT_DIR"), "/plugin_interfaces.rs"));
}

pub use account::Account;
pub use interfaces::PluginInterface;
pub use library::Library;
pub use manifest::Manifest;
pub use plugin::{Capability, Plugin};

use bottles_core::Directories;
use std::{
    fs,
    path::{Path, PathBuf},
};
use wasmtime::{Cache, CacheConfig, Engine, component::Linker};

use plugin::WasiState;

/// Manages package files and the engine used to load their components.
///
/// Packages live under [`Directories::plugins`] at `<id>/plugin.toml` and
/// `<id>/plugin.wasm`.
/// The catalog does not retain loaded handles or register providers with core.
///
/// # Examples
///
/// ```text
/// let plugins = bottles_plugin_host::Plugins::new(bottles.directories())?;
/// for manifest in plugins.list()? {
///     println!("{}: {}", manifest.id, manifest.name);
/// }
/// ```
pub struct Plugins {
    root: PathBuf,
    engine: Engine,
    linker: Linker<WasiState>,
}

impl Plugins {
    /// Creates the shared engine, linker, and persistent compilation cache.
    ///
    /// Adds WASIp3, outbound HTTP, and account input imports. Packages are loaded
    /// from [`Directories::plugins`]. Compiled components are cached across
    /// launches in the `wasmtime` subdirectory of [`Directories::cache_dir`].
    /// Package filesystem access starts with the package methods.
    ///
    /// # Errors
    ///
    /// Returns an error if the cache, engine, or host imports cannot be configured.
    ///
    /// # Examples
    ///
    /// ```text
    /// let plugins = bottles_plugin_host::Plugins::new(bottles.directories())?;
    /// ```
    pub fn new(directories: &Directories) -> wasmtime::Result<Self> {
        let mut cache = CacheConfig::new();
        cache.with_directory(directories.cache_dir().join("wasmtime"));
        let mut config = wasmtime::Config::new();
        config.wasm_component_model_async(true);
        config.cache(Some(Cache::new(cache)?));
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p3::add_to_linker(&mut linker)?;
        wasmtime_wasi_http::p3::add_to_linker(&mut linker)?;
        account::add_to_linker(&mut linker)?;
        Ok(Self {
            root: directories.plugins(),
            engine,
            linker,
        })
    }

    /// Reads the manifests of installed packages.
    ///
    /// Reads `plugin.toml` from every immediate child directory. Other entries
    /// are skipped. Results follow filesystem order and are not cached.
    ///
    /// # Errors
    ///
    /// Returns an error if the catalog cannot be read, an entry's type cannot
    /// be determined, or any directory's manifest cannot be read or parsed.
    /// A missing catalog directory is an error.
    ///
    /// # Examples
    ///
    /// ```text
    /// for manifest in plugins.list()? {
    ///     println!("{} {}", manifest.name, manifest.version);
    /// }
    /// ```
    pub fn list(&self) -> wasmtime::Result<Vec<Manifest>> {
        let mut manifests = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let manifest = Manifest::read(&entry.path().join("plugin.toml"))?;
                manifests.push(manifest);
            }
        }
        Ok(manifests)
    }

    /// Copies a package's manifest and component into the catalog.
    ///
    /// Uses the source manifest's ID as the destination path. Creates the
    /// directory and overwrites `plugin.toml` and `plugin.wasm`. Installation
    /// does not compile the component or change any loaded instance.
    ///
    /// # Errors
    ///
    /// Returns an error if the source manifest cannot be read or parsed, the
    /// destination directory cannot be created, or either file cannot be
    /// copied. Files already copied are left in place if a later copy fails.
    ///
    /// # Examples
    ///
    /// ```text
    /// let manifest = plugins.install(std::path::Path::new("dist/example"))?;
    /// let plugin = plugins.load(&manifest.id).await?;
    /// ```
    pub fn install(&self, source: &Path) -> wasmtime::Result<Manifest> {
        let manifest = Manifest::read(&source.join("plugin.toml"))?;
        let directory = self.root.join(&manifest.id);
        fs::create_dir_all(&directory)?;
        fs::copy(source.join("plugin.toml"), directory.join("plugin.toml"))?;
        fs::copy(source.join("plugin.wasm"), directory.join("plugin.wasm"))?;
        Ok(manifest)
    }

    /// Removes an installed package.
    ///
    /// Recursively removes the path formed by joining `id` to the catalog
    /// root. Loaded instances and core registrations are unaffected.
    ///
    /// # Errors
    ///
    /// Returns the filesystem error if the package directory cannot be
    /// removed, including when it does not exist.
    ///
    /// # Examples
    ///
    /// ```text
    /// plugins.uninstall("example")?;
    /// ```
    pub fn uninstall(&self, id: &str) -> wasmtime::Result<()> {
        Ok(fs::remove_dir_all(self.root.join(id))?)
    }

    /// Loads one package into a long-lived driver thread.
    ///
    /// Reads the manifest, compiles the component, and instantiates it before
    /// returning. Every load creates a new guest instance; casts of the returned
    /// handle share that instance. Loading does not register providers.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest cannot be read or parsed, the thread
    /// or its runtime cannot start, or the component cannot be read, compiled,
    /// linked, or instantiated. Also returns an error if the driver stops
    /// before reporting initialization.
    ///
    /// # Examples
    ///
    /// ```text
    /// let plugin = plugins.load("example").await?;
    /// if let Some(library) = plugin.cast::<bottles_plugin_host::Library>() {
    ///     bottles.library().register_provider(std::sync::Arc::new(library));
    /// }
    /// ```
    pub async fn load(&self, id: &str) -> wasmtime::Result<Plugin> {
        let directory = self.root.join(id);
        let manifest = Manifest::read(&directory.join("plugin.toml"))?;
        Plugin::load(
            manifest,
            directory.join("plugin.wasm"),
            self.engine.clone(),
            self.linker.clone(),
        )
        .await
    }
}
