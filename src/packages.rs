use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use next_config::Config;
use serde::{Deserialize, Serialize};
use tokio::{runtime::Handle, sync::Mutex};
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use crate::{
    CompiledPlugin, PluginError, PluginInfo, Runtime, exported_interfaces, parse_manifest,
};

type Result<T> = std::result::Result<T, PluginError>;

/// Captured metadata and code from one installed revision.
#[derive(Clone)]
pub struct LoadedPlugin {
    pub info: PluginInfo,
    pub component: Arc<CompiledPlugin>,
}

struct InstalledPlugin {
    info: PluginInfo,
    component: Option<Arc<CompiledPlugin>>,
}

#[derive(Default, Serialize, Deserialize, Config)]
#[config(version = 1)]
struct InstalledIndex {
    packages: BTreeMap<String, PluginInfo>,
}

/// A shared package catalog. Installed revisions are immutable; compilation is lazy.
pub struct Plugins {
    root: PathBuf,
    runtime: Runtime,
    installed: RwLock<BTreeMap<String, InstalledPlugin>>,
    lifecycle: Arc<Mutex<()>>,
    commits: TaskTracker,
    executor: Handle,
}

impl Plugins {
    pub async fn open(root: impl AsRef<Path>) -> Result<Arc<Self>> {
        let root = root.as_ref().to_owned();
        let index: InstalledIndex = match next_config::load(root.join("installed.toml")).await {
            Ok(index) => index,
            Err(next_config::error::Error::Io(error))
                if error.kind() == io::ErrorKind::NotFound =>
            {
                InstalledIndex::default()
            }
            Err(error) => return Err(error.into()),
        };
        Ok(Arc::new(Self {
            root,
            runtime: Runtime::new().map_err(PluginError::Runtime)?,
            installed: RwLock::new(
                index
                    .packages
                    .into_iter()
                    .map(|(id, info)| {
                        (
                            id,
                            InstalledPlugin {
                                info,
                                component: None,
                            },
                        )
                    })
                    .collect(),
            ),
            lifecycle: Arc::default(),
            commits: TaskTracker::new(),
            executor: Handle::try_current().map_err(|e| PluginError::Runtime(e.to_string()))?,
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
        let _lifecycle = self.lifecycle.lock().await;
        let (info, component) = {
            let installed = self.installed.read().unwrap();
            let entry = installed
                .get(id)
                .ok_or_else(|| PluginError::NotFound(id.into()))?;
            (entry.info.clone(), entry.component.clone())
        };
        let component = match component {
            Some(component) => component,
            None => {
                let bytes =
                    async_fs::read(self.revision_directory(info.revision).join("plugin.wasm"))
                        .await?;
                let component = Arc::new(
                    self.runtime
                        .compile(bytes)
                        .await
                        .map_err(PluginError::Runtime)?,
                );
                self.installed
                    .write()
                    .unwrap()
                    .get_mut(id)
                    .unwrap()
                    .component = Some(component.clone());
                component
            }
        };
        Ok(LoadedPlugin { info, component })
    }

    /// Prepare a revision, then own its publication even if the caller drops the future.
    pub async fn install(self: &Arc<Self>, source: &Path) -> Result<PluginInfo> {
        let manifest_text = async_fs::read_to_string(source.join("plugin.toml")).await?;
        let manifest = parse_manifest(&manifest_text)?;
        let bytes = async_fs::read(source.join("plugin.wasm")).await?;
        let interfaces = exported_interfaces(&bytes)?;
        let info = PluginInfo {
            manifest,
            interfaces,
            revision: Uuid::new_v4(),
        };
        let directory = self.revision_directory(info.revision);
        async_fs::create_dir_all(&directory).await?;
        async_fs::write(directory.join("plugin.toml"), manifest_text).await?;
        async_fs::write(directory.join("plugin.wasm"), bytes).await?;

        let guard = self.lifecycle.clone().lock_owned().await;
        let plugins = self.clone();
        self.commits
            .spawn_on(
                async move {
                    let _guard = guard;
                    let old = plugins
                        .publish(&info.manifest.id, Some(info.clone()))
                        .await?;
                    if let Some(old) = old {
                        plugins.cleanup(old.revision).await;
                    }
                    Ok(info)
                },
                &self.executor,
            )
            .await?
    }

    /// Publish removal before cleanup; obsolete files cannot restore membership.
    pub async fn uninstall(self: &Arc<Self>, id: &str) -> Result<()> {
        let guard = self.lifecycle.clone().lock_owned().await;
        let plugins = self.clone();
        let id = id.to_owned();
        self.commits
            .spawn_on(
                async move {
                    let _guard = guard;
                    let old = plugins.publish(&id, None).await?;
                    if let Some(old) = old {
                        plugins.cleanup(old.revision).await;
                    }
                    Ok(())
                },
                &self.executor,
            )
            .await?
    }

    // Caller owns lifecycle through saving the index and publishing the matching catalog.
    async fn publish(&self, id: &str, info: Option<PluginInfo>) -> Result<Option<PluginInfo>> {
        let mut index = InstalledIndex {
            packages: self
                .installed
                .read()
                .unwrap()
                .iter()
                .map(|(id, entry)| (id.clone(), entry.info.clone()))
                .collect(),
        };
        match &info {
            Some(info) => {
                index.packages.insert(id.into(), info.clone());
            }
            None => {
                index.packages.remove(id);
            }
        }
        next_config::save(self.root.join("installed.toml"), &index).await?;
        let mut installed = self.installed.write().unwrap();
        let old = match info {
            Some(info) => installed.insert(
                id.into(),
                InstalledPlugin {
                    info,
                    component: None,
                },
            ),
            None => installed.remove(id),
        };
        Ok(old.map(|p| p.info))
    }

    fn revision_directory(&self, revision: Uuid) -> PathBuf {
        self.root.join("revisions").join(revision.to_string())
    }

    async fn cleanup(&self, revision: Uuid) {
        if let Err(error) = async_fs::remove_dir_all(self.revision_directory(revision)).await {
            tracing::warn!(%revision, %error, "failed to remove obsolete plugin revision");
        }
    }
}
