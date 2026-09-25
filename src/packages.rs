use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use futures::StreamExt;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{PluginError, PluginInfo, Result, Runtime, parse_manifest};

use wasmtime::component::{Component, types::ComponentItem};

/// Immutable code and metadata captured from one installed package.
/// Sessions opened from this snapshot remain independent of later package changes.
pub struct CompiledPlugin {
    /// Metadata captured with this component.
    pub info: PluginInfo,
    component: Component,
}

impl CompiledPlugin {
    /// Returns the component for linking with the caller's imports and state.
    pub fn component(&self) -> &Component {
        &self.component
    }
}

enum InstalledPlugin {
    Uncompiled(PluginInfo),
    Compiled(Arc<CompiledPlugin>),
}

impl InstalledPlugin {
    fn info(&self) -> &PluginInfo {
        match self {
            Self::Uncompiled(info) => info,
            Self::Compiled(plugin) => &plugin.info,
        }
    }
}

/// A shared catalog of packages in `installed/<plugin-id>`. Compilation is lazy.
/// Open one catalog per root and share its `Arc`; independent writers are unsupported.
/// Callers must await mutations to finish publication and cleanup. Dropping a future
/// abandons remaining work. Failed replacement after removal may require reinstalling.
/// Loading snapshots and package publication are serialized. Retained compiled snapshots
/// and caller-owned sessions are unaffected by catalog changes or dropping the catalog.
pub struct Plugins {
    root: PathBuf,
    staging_root: PathBuf,
    runtime: Runtime,
    installed: RwLock<BTreeMap<String, InstalledPlugin>>,
    lifecycle: Mutex<()>,
}

impl Plugins {
    /// Scan installed metadata. Staging and installation must share a filesystem for rename.
    pub async fn open(root: impl AsRef<Path>, staging_root: impl AsRef<Path>) -> Result<Arc<Self>> {
        let root = root.as_ref().to_owned();
        let mut installed = BTreeMap::new();
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
                    installed.insert(info.manifest.id.clone(), InstalledPlugin::Uncompiled(info));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(Arc::new(Self {
            root,
            staging_root: staging_root.as_ref().to_owned(),
            runtime: Runtime::new()?,
            installed: RwLock::new(installed),
            lifecycle: Mutex::new(()),
        }))
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        self.installed
            .read()
            .unwrap()
            .values()
            .map(|p| p.info().clone())
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<PluginInfo> {
        self.installed
            .read()
            .unwrap()
            .get(id)
            .map(|p| p.info().clone())
    }

    /// Returns the current compiled snapshot, compiling it on first use.
    pub async fn load(&self, id: &str) -> Result<Arc<CompiledPlugin>> {
        self.load_component(id, false).await
    }

    /// Compiles a fresh snapshot of the installed code for subsequent loads.
    /// Existing snapshots and sessions keep their code and state; package files are unchanged.
    pub async fn reload(&self, id: &str) -> Result<Arc<CompiledPlugin>> {
        self.load_component(id, true).await
    }

    async fn load_component(&self, id: &str, reload: bool) -> Result<Arc<CompiledPlugin>> {
        if !reload {
            let installed = self.installed.read().unwrap();
            let entry = installed
                .get(id)
                .ok_or_else(|| PluginError::NotFound(id.into()))?;
            if let InstalledPlugin::Compiled(compiled) = entry {
                return Ok(compiled.clone());
            }
        }
        let _lifecycle = self.lifecycle.lock().await;
        let info = {
            let installed = self.installed.read().unwrap();
            let entry = installed
                .get(id)
                .ok_or_else(|| PluginError::NotFound(id.into()))?;
            if !reload && let InstalledPlugin::Compiled(compiled) = entry {
                return Ok(compiled.clone());
            }
            entry.info().clone()
        };
        let bytes = async_fs::read(self.directory(id).join("plugin.wasm")).await?;
        let component = self.runtime.compile(bytes).await?;
        let plugin = Arc::new(CompiledPlugin { info, component });
        *self.installed.write().unwrap().get_mut(id).unwrap() =
            InstalledPlugin::Compiled(plugin.clone());
        Ok(plugin)
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
            let mut installed = self.installed.write().unwrap();
            // Remove, rename and publish without yielding after withdrawal begins.
            installed.remove(&info.manifest.id);
            remove_directory(&directory)?;
            std::fs::rename(&workspace, &directory)?;
            installed.insert(
                info.manifest.id.clone(),
                InstalledPlugin::Compiled(Arc::new(CompiledPlugin {
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
        let mut installed = self.installed.write().unwrap();
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
