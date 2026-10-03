//! Installed WASIp3 plugins and adapters for Bottles core providers.

mod account;
mod library;
mod manifest;
mod plugin;

mod interfaces {
    include!(concat!(env!("OUT_DIR"), "/plugin_interfaces.rs"));
}

pub use account::Account;
pub use interfaces::PluginInterface;
pub use library::Library;
pub use manifest::Manifest;
pub use plugin::{Capability, Plugin};

use std::{
    fs,
    path::{Path, PathBuf},
};
use wasmtime::{Engine, component::Linker};

use plugin::WasiState;

/// A catalog of packages under `root/<id>/`.
pub struct Plugins {
    root: PathBuf,
    engine: Engine,
    linker: Linker<WasiState>,
}

impl Plugins {
    /// Creates the shared engine and linker.
    pub fn new(root: impl Into<PathBuf>) -> wasmtime::Result<Self> {
        let mut config = wasmtime::Config::new();
        config.wasm_component_model_async(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p3::add_to_linker(&mut linker)?;
        wasmtime_wasi_http::p3::add_to_linker(&mut linker)?;
        account::add_to_linker(&mut linker)?;
        Ok(Self {
            root: root.into(),
            engine,
            linker,
        })
    }

    /// Reads the manifests of installed packages.
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
    pub fn install(&self, source: &Path) -> wasmtime::Result<Manifest> {
        let manifest = Manifest::read(&source.join("plugin.toml"))?;
        let directory = self.root.join(&manifest.id);
        fs::create_dir_all(&directory)?;
        fs::copy(source.join("plugin.toml"), directory.join("plugin.toml"))?;
        fs::copy(source.join("plugin.wasm"), directory.join("plugin.wasm"))?;
        Ok(manifest)
    }

    /// Removes an installed package.
    pub fn uninstall(&self, id: &str) -> wasmtime::Result<()> {
        Ok(fs::remove_dir_all(self.root.join(id))?)
    }

    /// Loads one package into a long-lived driver thread.
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
