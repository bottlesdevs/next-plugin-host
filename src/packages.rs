use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{HostState, PluginError, PluginInfo, Result, Runtime, parse_manifest};

use wasmtime::component::{InstancePre, types::ComponentItem};

/// Captured metadata and code from one installed revision.
#[derive(Clone)]
pub struct LoadedPlugin {
    pub info: PluginInfo,
    pub component: InstancePre<HostState>,
}

struct InstalledPlugin {
    info: PluginInfo,
    component: Option<InstancePre<HostState>>,
}

/// A shared package catalog. Installed revisions are immutable; compilation is lazy.
pub struct Plugins {
    root: PathBuf,
    runtime: Runtime,
    installed: RwLock<BTreeMap<String, InstalledPlugin>>,
    lifecycle: Arc<Mutex<()>>,
}

impl Plugins {
    pub async fn open(root: impl AsRef<Path>, runtime: Runtime) -> Result<Arc<Self>> {
        let root = root.as_ref().to_owned();
        let index: BTreeMap<String, PluginInfo> =
            match async_fs::read_to_string(root.join("installed.toml")).await {
                Ok(source) => toml::from_str(&source)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
                Err(error) => return Err(error.into()),
            };
        Ok(Arc::new(Self {
            root,
            runtime,
            installed: RwLock::new(
                index
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
            lifecycle: Arc::new(Mutex::new(())),
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
        {
            let installed = self.installed.read().unwrap();
            let entry = installed
                .get(id)
                .ok_or_else(|| PluginError::NotFound(id.into()))?;
            if let Some(component) = &entry.component {
                return Ok(LoadedPlugin {
                    info: entry.info.clone(),
                    component: component.clone(),
                });
            }
        }
        let publication = self.lifecycle.lock().await;
        let (info, cached) = {
            let installed = self.installed.read().unwrap();
            let entry = installed
                .get(id)
                .ok_or_else(|| PluginError::NotFound(id.into()))?;
            (entry.info.clone(), entry.component.clone())
        };
        if let Some(component) = cached {
            return Ok(LoadedPlugin { info, component });
        }
        let bytes =
            async_fs::read(self.revision_directory(info.revision).join("plugin.wasm")).await?;
        drop(publication);
        let component = self.runtime.prepare(bytes).await?;
        if let Some(entry) = self.installed.write().unwrap().get_mut(id)
            && entry.info.revision == info.revision
        {
            entry.component = Some(component.clone());
        }
        Ok(LoadedPlugin { info, component })
    }

    /// Prepare a revision before committing it. Once entered, publication finishes
    /// even if the caller drops its future; no guest code runs during installation.
    pub async fn install(self: &Arc<Self>, source: &Path) -> Result<PluginInfo> {
        let manifest =
            parse_manifest(&async_fs::read_to_string(source.join("plugin.toml")).await?)?;
        let bytes = async_fs::read(source.join("plugin.wasm")).await?;
        let component = self.runtime.prepare(bytes.clone()).await?;
        let interfaces = component
            .component()
            .component_type()
            .exports(component.engine())
            .filter(|(_, export)| matches!(export.ty, ComponentItem::ComponentInstance(_)))
            .map(|(name, _)| name.to_owned())
            .collect();
        let info = PluginInfo {
            manifest,
            interfaces,
            revision: Uuid::new_v4(),
        };
        let publication = self.lifecycle.clone().lock_owned().await;
        let plugins = self.clone();
        blocking::unblock(move || {
            let directory = plugins.revision_directory(info.revision);
            std::fs::create_dir_all(&directory)?;
            std::fs::write(directory.join("plugin.wasm"), bytes)?;
            let old = plugins.publish(
                &info.manifest.id,
                Some(InstalledPlugin {
                    info: info.clone(),
                    component: Some(component),
                }),
            )?;
            drop(publication);
            if let Some(old) = old {
                plugins.cleanup(old.revision);
            }
            Ok(info)
        })
        .await
    }

    /// Remove catalog membership. Already captured revisions may finish their work.
    /// Publication finishes even if the caller drops its future once it has begun.
    pub async fn uninstall(self: &Arc<Self>, id: &str) -> Result<()> {
        let publication = self.lifecycle.clone().lock_owned().await;
        let plugins = self.clone();
        let id = id.to_owned();
        blocking::unblock(move || {
            let old = plugins.publish(&id, None)?;
            drop(publication);
            if let Some(old) = old {
                plugins.cleanup(old.revision);
            }
            Ok(())
        })
        .await
    }

    // Caller holds the publication guard on a blocking worker through disk and memory changes.
    fn publish(&self, id: &str, entry: Option<InstalledPlugin>) -> Result<Option<PluginInfo>> {
        let mut index: BTreeMap<String, PluginInfo> = self
            .installed
            .read()
            .unwrap()
            .iter()
            .map(|(id, entry)| (id.clone(), entry.info.clone()))
            .collect();
        match &entry {
            Some(entry) => {
                index.insert(id.into(), entry.info.clone());
            }
            None => {
                index.remove(id);
            }
        }
        std::fs::create_dir_all(&self.root)?;
        let temporary = self.root.join("installed.tmp");
        std::fs::write(&temporary, toml::to_string_pretty(&index)?)?;
        std::fs::rename(temporary, self.root.join("installed.toml"))?;
        let mut installed = self.installed.write().unwrap();
        let old = match entry {
            Some(entry) => installed.insert(id.into(), entry),
            None => installed.remove(id),
        };
        Ok(old.map(|entry| entry.info))
    }

    fn revision_directory(&self, revision: Uuid) -> PathBuf {
        self.root.join("revisions").join(revision.to_string())
    }

    fn cleanup(&self, revision: Uuid) {
        if let Err(error) = std::fs::remove_dir_all(self.revision_directory(revision)) {
            tracing::warn!(%revision, %error, "failed to remove obsolete plugin revision");
        }
    }
}
