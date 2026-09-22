use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use futures::StreamExt;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{PluginError, PluginInfo, Result, Runtime, parse_manifest, runtime::Worker};

use wasmtime::component::types::ComponentItem;

/// Captured metadata and a shared persistent runtime for one installed plugin.
/// Calls are serialized; retired handles reject new calls and never change instance identity.
#[derive(Clone)]
pub struct LoadedPlugin {
    pub info: PluginInfo,
    pub(crate) worker: Arc<Worker>,
}

struct InstalledPlugin {
    info: PluginInfo,
    loaded: Option<LoadedPlugin>,
}

impl Drop for InstalledPlugin {
    fn drop(&mut self) {
        if let Some(plugin) = &self.loaded {
            plugin.worker.sender.close_channel();
        }
    }
}

/// A shared catalog of packages in `installed/<plugin-id>`. Compilation is lazy.
/// Open one catalog per root and share its `Arc`; independent writers are unsupported.
/// Callers must await mutations to finish publication and cleanup. Dropping a future
/// abandons remaining work. Failed replacement after removal may require reinstalling.
/// Dropping the last catalog owner retires its runtimes, including retained loaded handles.
/// Runtime startup and package publication are serialized; calls to loaded plugins remain independent.
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
                    installed.insert(
                        info.manifest.id.clone(),
                        InstalledPlugin { info, loaded: None },
                    );
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
            .map(|p| p.info.clone())
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<PluginInfo> {
        self.installed
            .read()
            .unwrap()
            .get(id)
            .map(|p| p.info.clone())
    }

    pub async fn load(&self, id: &str) -> Result<LoadedPlugin> {
        self.load_runtime(id, false).await
    }

    /// Retire the current runtime and start a replacement using the installed code.
    /// This changes no package files.
    pub async fn reload(&self, id: &str) -> Result<LoadedPlugin> {
        self.load_runtime(id, true).await
    }

    async fn load_runtime(&self, id: &str, reload: bool) -> Result<LoadedPlugin> {
        if !reload {
            let installed = self.installed.read().unwrap();
            let entry = installed
                .get(id)
                .ok_or_else(|| PluginError::NotFound(id.into()))?;
            if let Some(loaded) = &entry.loaded {
                return Ok(loaded.clone());
            }
        }
        let _lifecycle = self.lifecycle.lock().await;
        let info = {
            let installed = self.installed.read().unwrap();
            let entry = installed
                .get(id)
                .ok_or_else(|| PluginError::NotFound(id.into()))?;
            if !reload && let Some(loaded) = &entry.loaded {
                return Ok(loaded.clone());
            }
            entry.info.clone()
        };
        let bytes = async_fs::read(self.directory(id).join("plugin.wasm")).await?;
        let component = self.runtime.compile(bytes).await?;
        let pre = self.runtime.link(&component)?;
        if reload {
            let mut installed = self.installed.write().unwrap();
            let entry = installed.get_mut(id).unwrap();
            *entry = InstalledPlugin {
                info: info.clone(),
                loaded: None,
            };
        }
        let plugin = LoadedPlugin {
            info,
            worker: Arc::new(Worker::new(pre).await?),
        };
        self.installed.write().unwrap().get_mut(id).unwrap().loaded = Some(plugin.clone());
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
            // Retire, remove, rename and publish without yielding after withdrawal begins.
            drop(installed.remove(&info.manifest.id));
            remove_directory(&directory)?;
            std::fs::rename(&workspace, &directory)?;
            installed.insert(
                info.manifest.id.clone(),
                InstalledPlugin {
                    info: info.clone(),
                    loaded: None,
                },
            );
            Ok(info)
        }
        .await;
        if result.is_err() {
            let _ = async_fs::remove_dir_all(&workspace).await;
        }
        result
    }

    /// Remove catalog membership and retire the runtime. Already accepted calls may finish.
    pub async fn uninstall(&self, id: &str) -> Result<()> {
        let directory = self.directory(id);
        let _publication = self.lifecycle.lock().await;
        let mut installed = self.installed.write().unwrap();
        drop(installed.remove(id));
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
