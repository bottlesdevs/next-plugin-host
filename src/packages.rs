use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{HostState, PluginError, PluginInfo, Result, Runtime, parse_manifest};

use wasmtime::component::{Component, InstancePre, types::ComponentItem};

/// Captured metadata and code from one installed revision.
#[derive(Clone)]
pub struct LoadedPlugin {
    pub info: PluginInfo,
    pub component: InstancePre<HostState>,
}

struct InstalledPlugin {
    info: PluginInfo,
    component: Option<Component>,
}

/// A shared package catalog. Installed revisions are immutable; compilation is lazy.
/// Open one catalog per root and share its `Arc`; independent writers are unsupported.
/// Callers must await mutations to finish publication and cleanup. Dropping a future
/// abandons remaining work and does not roll back an already published revision.
pub struct Plugins {
    root: PathBuf,
    runtime: Runtime,
    installed: RwLock<BTreeMap<String, InstalledPlugin>>,
    lifecycle: Mutex<()>,
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
        {
            let installed = self.installed.read().unwrap();
            let entry = installed
                .get(id)
                .ok_or_else(|| PluginError::NotFound(id.into()))?;
            if let Some(component) = &entry.component {
                return Ok(LoadedPlugin {
                    info: entry.info.clone(),
                    component: self.runtime.link(component)?,
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
            drop(publication);
            return Ok(LoadedPlugin {
                info,
                component: self.runtime.link(&component)?,
            });
        }
        let bytes =
            async_fs::read(self.revision_directory(info.revision).join("plugin.wasm")).await?;
        let component = self.runtime.compile(bytes).await?;
        self.installed
            .write()
            .unwrap()
            .get_mut(id)
            .unwrap()
            .component = Some(component.clone());
        drop(publication);
        Ok(LoadedPlugin {
            info,
            component: self.runtime.link(&component)?,
        })
    }

    /// Compile and inspect a revision without resolving application imports or running guest code.
    pub async fn install(&self, source: &Path) -> Result<PluginInfo> {
        let manifest =
            parse_manifest(&async_fs::read_to_string(source.join("plugin.toml")).await?)?;
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
            revision: Uuid::new_v4(),
        };
        let publication = self.lifecycle.lock().await;
        let directory = self.revision_directory(info.revision);
        let result = async {
            async_fs::create_dir_all(&directory).await?;
            async_fs::write(directory.join("plugin.wasm"), bytes).await?;
            self.publish(
                &info.manifest.id,
                Some(InstalledPlugin {
                    info: info.clone(),
                    component: Some(component),
                }),
            )
            .await
        }
        .await;
        drop(publication);
        match result {
            Ok(old) => {
                if let Some(old) = old {
                    self.cleanup(old.revision).await;
                }
                Ok(info)
            }
            Err(error) => {
                self.cleanup(info.revision).await;
                Err(error)
            }
        }
    }

    /// Remove catalog membership. Already captured revisions may finish their work.
    pub async fn uninstall(&self, id: &str) -> Result<()> {
        let publication = self.lifecycle.lock().await;
        let old = self.publish(id, None).await?;
        drop(publication);
        if let Some(old) = old {
            self.cleanup(old.revision).await;
        }
        Ok(())
    }

    // Caller holds the publication guard through disk and memory changes.
    async fn publish(
        &self,
        id: &str,
        entry: Option<InstalledPlugin>,
    ) -> Result<Option<PluginInfo>> {
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
        async_fs::create_dir_all(&self.root).await?;
        let temporary = self.root.join(format!("installed-{}.tmp", Uuid::new_v4()));
        let result = async {
            async_fs::write(&temporary, toml::to_string_pretty(&index)?).await?;
            // No await between the disk commit and publishing the same state in memory.
            std::fs::rename(&temporary, self.root.join("installed.toml"))?;
            let mut installed = self.installed.write().unwrap();
            let old = match entry {
                Some(entry) => installed.insert(id.into(), entry),
                None => installed.remove(id),
            };
            Ok(old.map(|entry| entry.info))
        }
        .await;
        if result.is_err() {
            let _ = async_fs::remove_file(temporary).await;
        }
        result
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
