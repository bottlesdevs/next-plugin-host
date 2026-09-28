use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use futures::StreamExt;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    Plugin, PluginError, PluginInfo, Result, Runtime, WasiState, parse_manifest,
    runtime::{PluginSession, add_to_linker},
};

use wasmtime::{
    Store,
    component::{Component, Instance, Linker, types::ComponentItem},
};
use wasmtime_wasi::WasiCtxBuilder;

/// Immutable code and metadata captured from one installed package.
/// Sessions created from this snapshot survive later package changes.
pub(crate) struct CompiledPlugin {
    /// Metadata captured with this component.
    pub(crate) info: PluginInfo,
    component: Component,
}

#[derive(Clone)]
enum PluginEntry {
    Installed(PluginInfo),
    Compiled(Arc<CompiledPlugin>),
    Instantiated(Arc<PluginSession>),
}

impl PluginEntry {
    fn info(&self) -> &PluginInfo {
        match self {
            Self::Installed(info) => info,
            Self::Compiled(plugin) => &plugin.info,
            Self::Instantiated(session) => &session.compiled.info,
        }
    }
}

/// A shared catalog of packages in `installed/<plugin-id>`. Compilation is lazy.
/// Open one catalog per root and share its `Arc`; independent writers are unsupported.
/// Callers must await mutations to finish publication and cleanup. Dropping a future
/// abandons remaining work. Failed replacement after removal may require reinstalling.
/// Loading sessions and package publication are serialized. Retained handles are
/// unaffected by catalog changes or dropping the catalog.
pub struct Plugins {
    root: PathBuf,
    staging_root: PathBuf,
    runtime: Runtime,
    entries: RwLock<HashMap<String, PluginEntry>>,
    lifecycle: Mutex<()>,
}

impl Plugins {
    /// Scan installed metadata. Staging and installation must share a filesystem for rename.
    pub async fn open(root: impl AsRef<Path>, staging_root: impl AsRef<Path>) -> Result<Arc<Self>> {
        let root = root.as_ref().to_owned();
        let mut entries = HashMap::new();
        match async_fs::read_dir(root.join("installed")).await {
            Ok(mut directories) => {
                while let Some(directory) = directories.next().await {
                    let directory = directory?;
                    if !directory.file_type().await?.is_dir() {
                        continue;
                    }
                    let info: PluginInfo = toml::from_str(
                        &async_fs::read_to_string(directory.path().join("plugin.toml")).await?,
                    )?;
                    entries.insert(info.manifest.id.clone(), PluginEntry::Installed(info));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(Arc::new(Self {
            root,
            staging_root: staging_root.as_ref().to_owned(),
            runtime: Runtime::new()?,
            entries: RwLock::new(entries),
            lifecycle: Mutex::new(()),
        }))
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        self.entries
            .read()
            .unwrap()
            .values()
            .map(|p| p.info().clone())
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<PluginInfo> {
        self.entries
            .read()
            .unwrap()
            .get(id)
            .map(|p| p.info().clone())
    }

    /// Attaches typed bindings to the shared session for `info`'s plugin ID.
    /// The first load instantiates the component with the supplied imports;
    /// subsequent loads reuse that import environment and guest state.
    /// A closed session is replaced on the next load. Existing handles retain
    /// their original generation after reload or package replacement.
    /// Poll this future in the caller's Tokio runtime with I/O and time enabled.
    ///
    /// # Errors
    ///
    /// Returns an error if the ID is no longer installed, the component cannot
    /// compile or instantiate, or either supplied callback fails.
    pub async fn load<Bindings: Send + Sync>(
        &self,
        info: &PluginInfo,
        register_imports: impl FnOnce(&mut Linker<WasiState>) -> wasmtime::Result<()> + Send,
        load_exports: impl FnOnce(&mut Store<WasiState>, &Instance) -> wasmtime::Result<Bindings> + Send,
    ) -> Result<Plugin<Bindings>> {
        let id = &info.manifest.id;
        let _lifecycle = self.lifecycle.lock().await;
        let entry = self
            .entries
            .read()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| PluginError::NotFound(id.clone()))?;
        if let PluginEntry::Instantiated(session) = &entry {
            let mut store = session.store.lock().await;
            if let Some(store) = store.as_mut() {
                let bindings = load_exports(store, &session.instance)?;
                return Ok(Plugin::new(session.clone(), bindings));
            }
        }
        let compiled = match entry {
            PluginEntry::Installed(info) => {
                let compiled = self.compile_component(id, info).await?;
                *self.entries.write().unwrap().get_mut(id).unwrap() =
                    PluginEntry::Compiled(compiled.clone());
                compiled
            }
            PluginEntry::Compiled(compiled) => compiled,
            PluginEntry::Instantiated(session) => session.compiled.clone(),
        };
        let mut linker = Linker::new(compiled.component.engine());
        add_to_linker(&mut linker)?;
        register_imports(&mut linker)?;
        let pre = linker.instantiate_pre(&compiled.component)?;
        let state = WasiState::new(WasiCtxBuilder::new().build());
        let session = Arc::new(PluginSession::new(compiled, &pre, state).await?);
        let bindings = {
            let mut store = session.store.lock().await;
            load_exports(store.as_mut().unwrap(), &session.instance)?
        };
        *self.entries.write().unwrap().get_mut(id).unwrap() =
            PluginEntry::Instantiated(session.clone());
        Ok(Plugin::new(session, bindings))
    }

    /// Compiles a fresh snapshot of the installed code for subsequent loads.
    /// Existing snapshots and sessions keep their code and state; package files are unchanged.
    pub async fn reload(&self, id: &str) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        let info = self
            .entries
            .read()
            .unwrap()
            .get(id)
            .ok_or_else(|| PluginError::NotFound(id.into()))?
            .info()
            .clone();
        let compiled = self.compile_component(id, info).await?;
        *self.entries.write().unwrap().get_mut(id).unwrap() = PluginEntry::Compiled(compiled);
        Ok(())
    }

    async fn compile_component(&self, id: &str, info: PluginInfo) -> Result<Arc<CompiledPlugin>> {
        let bytes = async_fs::read(self.directory(id).join("plugin.wasm")).await?;
        let component = self.runtime.compile(bytes).await?;
        Ok(Arc::new(CompiledPlugin { info, component }))
    }

    /// Prepare a complete package without running guest code, then replace its installed directory.
    /// Preparation failures preserve the old installation; replacement failures may require reinstalling.
    pub async fn install(&self, source: &Path) -> Result<PluginInfo> {
        let manifest =
            parse_manifest(&async_fs::read_to_string(source.join("plugin.toml")).await?)?;
        let directory = self.directory(&manifest.id);
        let bytes = async_fs::read(source.join("plugin.wasm")).await?;
        let component = self.runtime.compile(bytes.clone()).await?;
        let interfaces = component
            .component_type()
            .exports(component.engine())
            .filter(|(_, export)| matches!(export.ty, ComponentItem::ComponentInstance(_)))
            .map(|(name, _)| name.to_owned())
            .collect();
        let info = PluginInfo {
            manifest,
            interfaces,
        };
        let workspace = self.staging_root.join(Uuid::new_v4().to_string());
        async_fs::create_dir_all(&workspace).await?;
        let result = async {
            async_fs::write(workspace.join("plugin.wasm"), bytes).await?;
            async_fs::write(
                workspace.join("plugin.toml"),
                toml::to_string_pretty(&info)?,
            )
            .await?;
            async_fs::create_dir_all(self.root.join("installed")).await?;
            let _publication = self.lifecycle.lock().await;
            let mut installed = self.entries.write().unwrap();
            // Remove, rename and publish without yielding after withdrawal begins.
            installed.remove(&info.manifest.id);
            remove_directory(&directory)?;
            std::fs::rename(&workspace, &directory)?;
            installed.insert(
                info.manifest.id.clone(),
                PluginEntry::Compiled(Arc::new(CompiledPlugin {
                    info: info.clone(),
                    component,
                })),
            );
            Ok(info)
        }
        .await;
        if result.is_err() {
            let _ = async_fs::remove_dir_all(&workspace).await;
        }
        result
    }

    /// Removes the package without affecting retained snapshots or sessions.
    pub async fn uninstall(&self, id: &str) -> Result<()> {
        let directory = self.directory(id);
        let _publication = self.lifecycle.lock().await;
        let mut installed = self.entries.write().unwrap();
        installed.remove(id);
        remove_directory(&directory)?;
        Ok(())
    }

    fn directory(&self, id: &str) -> PathBuf {
        self.root.join("installed").join(id)
    }
}

fn remove_directory(path: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
