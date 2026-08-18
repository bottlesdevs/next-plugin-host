use std::{path::PathBuf, sync::OnceLock, time::Duration};

use bottles_core::AccountIdentity;
use tokio::sync::Mutex;
use uuid::NonNilUuid;
use wasmtime::{
    Engine, Store, StoreLimitsBuilder,
    component::{Component, Linker, ResourceTable},
};
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::{
    Error, PluginManifest, Result, bindings::Plugin as GuestPlugin, package::ValidatedPackage,
};

const INITIALIZATION_FUEL: u64 = 10_000_000;
const INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Compilation and linking state shared by all plugin instances.
///
/// Guest stores are created separately and never share mutable state.
struct Runtime {
    engine: Engine,
    linker: Linker<HostState>,
}

/// Provides serialized access to one initialized plugin instance.
pub(crate) struct PluginHandle {
    manifest: PluginManifest,
    instance: Mutex<Option<PluginInstance>>,
}

struct PluginInstance {
    store: Store<HostState>,
    guest: GuestPlugin,
}

/// Store-owned capabilities and resource limits for one plugin instance.
///
/// Filesystem access is restricted to the plugin's persistent work directory,
/// and raw TCP and UDP sockets are unavailable.
pub(crate) struct HostState {
    pub table: ResourceTable,
    pub wasi: WasiCtx,
    pub limits: wasmtime::StoreLimits,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl HostState {
    /// Creates the per-plugin WASI sandbox and persistent data mount.
    async fn new(data_directory: PathBuf, plugin_id: NonNilUuid) -> Result<Self> {
        let data_directory = data_directory.join(plugin_id.to_string());
        async_fs::create_dir_all(&data_directory).await?;
        let data_directory = async_fs::canonicalize(data_directory).await?;
        let mut wasi = WasiCtxBuilder::new();
        wasi.initial_cwd(".")
            .allow_tcp(false)
            .allow_udp(false)
            .max_random_size(1024 * 1024);
        wasi.preopened_dir(&data_directory, ".", DirPerms::all(), FilePerms::all())
            .map_err(|error| Error::Host(error.to_string()))?;
        Ok(Self {
            table: ResourceTable::new(),
            wasi: wasi.build(),
            limits: StoreLimitsBuilder::new()
                .memory_size(64 * 1024 * 1024)
                .memories(4)
                .instances(8)
                .tables(16)
                .table_elements(100_000)
                .trap_on_grow_failure(true)
                .build(),
        })
    }
}

fn runtime() -> Result<&'static Runtime> {
    static RUNTIME: OnceLock<std::result::Result<Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            let mut config = wasmtime::Config::new();
            config
                .consume_fuel(true)
                .epoch_interruption(true)
                .wasm_component_model(true);
            let engine = Engine::new(&config).map_err(|error| error.to_string())?;
            let ticker = engine.clone();
            // Advancing epochs periodically yields guest execution to its executor.
            std::thread::Builder::new()
                .name("bottles-plugin-epoch".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(Duration::from_millis(100));
                        ticker.increment_epoch();
                    }
                })
                .map_err(|error| error.to_string())?;
            let mut linker = Linker::new(&engine);
            wasmtime_wasi::p2::add_to_linker_async(&mut linker)
                .map_err(|error| error.to_string())?;
            Ok(Runtime { engine, linker })
        })
        .as_ref()
        .map_err(|error| Error::Host(error.clone()))
}

impl PluginHandle {
    /// Compiles and initializes a component in a fresh, isolated store.
    ///
    /// An instance is returned only after initialization completes within the
    /// configured fuel, memory, and wall-clock limits.
    pub async fn load(data_directory: PathBuf, package: &ValidatedPackage) -> Result<Self> {
        let runtime = runtime()?;
        let plugin_id = package.manifest.id;
        let component = Component::from_binary(&runtime.engine, &package.component)
            .map_err(|error| Error::Load(plugin_id, error.to_string()))?;
        let state = HostState::new(data_directory, package.manifest.id)
            .await
            .map_err(|error| Error::Load(plugin_id, error.to_string()))?;
        let mut store = Store::new(&runtime.engine, state);
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(INITIALIZATION_FUEL)
            .map_err(|error| Error::Load(plugin_id, error.to_string()))?;
        store.set_epoch_deadline(1);
        store.epoch_deadline_async_yield_and_update(1);
        let guest = GuestPlugin::instantiate_async(&mut store, &component, &runtime.linker)
            .await
            .map_err(|error| Error::Load(plugin_id, error.to_string()))?;
        futures_lite::future::race(
            async {
                guest
                    .call_init_plugin(&mut store)
                    .await
                    .map_err(|error| Error::Load(plugin_id, error.to_string()))
            },
            async {
                async_io::Timer::after(INITIALIZATION_TIMEOUT).await;
                Err(Error::Load(plugin_id, "initialization timed out".into()))
            },
        )
        .await?;
        Ok(Self {
            manifest: package.manifest.clone(),
            instance: Mutex::new(Some(PluginInstance { store, guest })),
        })
    }

    pub(crate) fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    /// Calls the storefront account contribution on this plugin instance.
    pub async fn link_account(&self, profile_id: String) -> Result<AccountIdentity> {
        let mut instance = self.instance.lock().await;
        let instance = instance
            .as_mut()
            .ok_or_else(|| Error::Callback(self.manifest.id, "plugin instance is closed".into()))?;
        instance
            .store
            .set_fuel(INITIALIZATION_FUEL)
            .map_err(|error| Error::Callback(self.manifest.id, error.to_string()))?;

        let result = futures_lite::future::race(
            async {
                instance
                    .guest
                    .call_link_account(&mut instance.store, &profile_id)
                    .await
                    .map_err(|error| Error::Callback(self.manifest.id, error.to_string()))
            },
            async {
                async_io::Timer::after(INITIALIZATION_TIMEOUT).await;
                Err(Error::Callback(
                    self.manifest.id,
                    "link-account timed out".into(),
                ))
            },
        )
        .await?
        .map_err(|message| Error::Callback(self.manifest.id, message))?;

        Ok(AccountIdentity {
            account_id: result.account_id,
            display_name: result.display_name,
        })
    }

    /// Prevents further callbacks and releases the store after an active call finishes.
    pub async fn close(&self) {
        self.instance.lock().await.take();
    }
}
