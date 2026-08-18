use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use bottles_core::{Bottles, Directories};
use futures_lite::StreamExt;
use serde::Serialize;
use tokio::sync::Mutex;
use uuid::{NonNilUuid, Uuid};

use crate::{
    Error, PluginManifest, Result,
    package::{ValidatedPackage, activate_package, build_source, validate_package},
    runtime::PluginInstance,
};

/// Owns installed-plugin discovery and the initialized instances.
///
/// Installation is the activation mechanism: every valid installed package is
/// initialized at startup.
pub struct PluginManager {
    directories: Directories,
    plugins: Mutex<HashMap<NonNilUuid, PluginEntry>>,
}

impl PluginManager {
    fn root_directory(&self) -> PathBuf {
        self.directories.data_dir().join("plugins")
    }

    fn installed_directory(&self) -> PathBuf {
        self.root_directory().join("installed")
    }

    fn data_directory(&self) -> PathBuf {
        self.root_directory().join("data")
    }

    fn staging_directory(&self) -> PathBuf {
        self.root_directory().join("staging")
    }

    fn package_directory(&self, plugin_id: NonNilUuid) -> PathBuf {
        self.installed_directory().join(plugin_id.to_string())
    }

    /// Discovers and initializes packages beneath Bottles' plugin directory.
    ///
    /// Invalid packages are skipped independently. Valid packages whose
    /// components fail to load remain visible with [`PluginStatus::Failed`].
    pub async fn open(bottles: &Bottles) -> Result<Self> {
        let manager = Self {
            directories: bottles.directories().clone(),
            plugins: Mutex::new(HashMap::new()),
        };
        async_fs::create_dir_all(manager.installed_directory()).await?;
        async_fs::create_dir_all(manager.data_directory()).await?;
        async_fs::create_dir_all(manager.staging_directory()).await?;
        manager.discover().await?;
        Ok(manager)
    }

    /// Returns an unordered snapshot of all discovered plugins.
    pub async fn list(&self) -> Vec<PluginInfo> {
        self.plugins
            .lock()
            .await
            .values()
            .map(|entry| PluginInfo {
                manifest: entry.manifest.clone(),
                status: entry.state.status(),
            })
            .collect()
    }

    /// Initializes a fresh instance from the installed package, then replaces
    /// the current instance only after initialization succeeds.
    pub async fn reload(&self, plugin_id: NonNilUuid) -> Result<()> {
        if !self.plugins.lock().await.contains_key(&plugin_id) {
            return Err(Error::PluginNotFound(plugin_id));
        }
        let package = validate_package(&self.package_directory(plugin_id)).await?;
        if package.manifest.id != plugin_id {
            return Err(Error::InvalidManifest(format!(
                "installed directory {plugin_id:?} contains plugin {:?}",
                package.manifest.id
            )));
        }
        let instance = PluginInstance::load(self.data_directory(), &package).await?;
        let old = {
            let mut plugins = self.plugins.lock().await;
            let entry = plugins
                .get_mut(&plugin_id)
                .ok_or(Error::PluginNotFound(plugin_id))?;

            std::mem::replace(
                entry,
                PluginEntry {
                    manifest: package.manifest,
                    state: PluginState::Enabled {
                        _instance: instance,
                    },
                },
            )
        };
        drop(old); // Otherwise the replaced store lives until reload returns.
        Ok(())
    }

    /// Drops the resident instance before removing its package and private data.
    /// Missing directories are accepted so interrupted removals can be retried.
    pub async fn uninstall(&self, plugin_id: NonNilUuid) -> Result<()> {
        let old = self.plugins.lock().await.remove(&plugin_id);
        drop(old); // Otherwise WASI resources remain open while their files are deleted.
        remove_directory(self.package_directory(plugin_id)).await?;
        remove_directory(self.data_directory().join(plugin_id.to_string())).await?;
        Ok(())
    }

    /// Validates and initializes an unpacked runtime package before activation.
    pub async fn install(&self, package_directory: &Path) -> Result<PluginInfo> {
        let package = validate_package(package_directory).await?;
        self.replace_package(package).await
    }

    /// Builds a source checkout in place and activates its resulting component.
    /// Repeating this operation rebuilds and replaces the installed instance.
    pub async fn dev_install(&self, source: &Path) -> Result<PluginInfo> {
        let package = build_source(source).await?;
        self.replace_package(package).await
    }

    async fn discover(&self) -> Result<()> {
        let mut directories = async_fs::read_dir(self.installed_directory()).await?;
        let mut discovered = HashMap::new();
        while let Some(entry) = directories.next().await.transpose()? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }

            let Some(plugin_id) = entry
                .file_name()
                .to_str()
                .and_then(|name| Uuid::parse_str(name).ok())
                .and_then(NonNilUuid::new)
            else {
                tracing::warn!(path = %entry.path().display(), "skipping plugin directory whose name is not a UUID");
                continue;
            };
            let directory = entry.path();
            let package = match validate_package(&directory).await {
                Ok(package) => package,
                Err(error) => {
                    tracing::warn!(path = %directory.display(), "skipping invalid plugin: {error}");
                    continue;
                }
            };
            if plugin_id != package.manifest.id {
                tracing::warn!(
                    path = %directory.display(),
                    plugin_id = %package.manifest.id,
                    "skipping plugin whose directory does not match its ID"
                );
                continue;
            }
            let state = match PluginInstance::load(self.data_directory(), &package).await {
                Ok(instance) => PluginState::Enabled {
                    _instance: instance,
                },
                Err(error) => PluginState::Failed(error.to_string()),
            };
            discovered.insert(
                package.manifest.id,
                PluginEntry {
                    manifest: package.manifest,
                    state,
                },
            );
        }
        *self.plugins.lock().await = discovered;
        Ok(())
    }

    /// Initializes before changing either the installed files or live instance.
    async fn replace_package(&self, package: ValidatedPackage) -> Result<PluginInfo> {
        let instance = PluginInstance::load(self.data_directory(), &package).await?;
        activate_package(
            &package,
            &self.installed_directory(),
            &self.staging_directory(),
        )
        .await?;
        let info = PluginInfo {
            manifest: package.manifest.clone(),
            status: PluginStatus::Enabled,
        };
        let old = self.plugins.lock().await.insert(
            package.manifest.id,
            PluginEntry {
                manifest: package.manifest,
                state: PluginState::Enabled {
                    _instance: instance,
                },
            },
        );
        drop(old); // Otherwise the replaced store lives until installation returns.
        Ok(info)
    }
}

/// Outcome of loading an installed plugin.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginStatus {
    /// Initialization succeeded and the instance remains resident.
    Enabled,
    /// The package remains installed, but compilation or initialization failed.
    Failed(String),
}

enum PluginState {
    Enabled { _instance: PluginInstance },
    Failed(String),
}

impl PluginState {
    fn status(&self) -> PluginStatus {
        match self {
            Self::Enabled { .. } => PluginStatus::Enabled,
            Self::Failed(error) => PluginStatus::Failed(error.clone()),
        }
    }
}

/// Snapshot of an installed plugin and its current load outcome.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PluginInfo {
    /// Manifest accepted during package discovery or installation.
    pub manifest: PluginManifest,
    /// Whether initialization produced a resident store or failed.
    pub status: PluginStatus,
}

struct PluginEntry {
    manifest: PluginManifest,
    state: PluginState,
}

async fn remove_directory(path: PathBuf) -> Result<()> {
    match async_fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
