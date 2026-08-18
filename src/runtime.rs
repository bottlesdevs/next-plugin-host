use std::{path::PathBuf, sync::OnceLock, time::Duration};

use wasmtime::{
    Engine, Store, StoreLimitsBuilder,
    component::{Component, Linker, ResourceTable},
};
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::{Error, PluginManifest, Result, bindings::Plugin as GuestPlugin};

const INITIALIZATION_FUEL: u64 = 10_000_000;
const INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Compilation and linking state shared by all plugin instances.
///
/// Guest stores are created separately and never share mutable state.
struct Runtime {
    engine: Engine,
    linker: Linker<HostState>,
}

/// Keeps an initialized component's memory and WASI resources alive.
pub(crate) struct PluginInstance {
    _store: Store<HostState>,
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
            // Advancing epochs makes non-yielding guest code interruptible.
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

impl PluginInstance {
    /// Compiles and initializes a component in a fresh, isolated store.
    ///
    /// An instance is returned only after initialization completes within the
    /// configured fuel, memory, and wall-clock limits.
    pub async fn load(
        data_directory: PathBuf,
        manifest: &PluginManifest,
        component: &[u8],
    ) -> Result<Self> {
        let runtime = runtime()?;
        let plugin_id = manifest.id.to_string();
        let component = Component::from_binary(&runtime.engine, component)
            .map_err(|error| Error::Load(plugin_id.clone(), error.to_string()))?;
        let state = build_state(data_directory, manifest)
            .await
            .map_err(|error| Error::Load(plugin_id.clone(), error.to_string()))?;
        let mut store = Store::new(&runtime.engine, state);
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(INITIALIZATION_FUEL)
            .map_err(|error| Error::Load(plugin_id.clone(), error.to_string()))?;
        store.set_epoch_deadline(300);
        let guest = GuestPlugin::instantiate_async(&mut store, &component, &runtime.linker)
            .await
            .map_err(|error| Error::Load(plugin_id.clone(), error.to_string()))?;
        futures_lite::future::race(
            async {
                guest
                    .call_init_plugin(&mut store)
                    .await
                    .map_err(|error| Error::Load(plugin_id.clone(), error.to_string()))
            },
            async {
                async_io::Timer::after(INITIALIZATION_TIMEOUT).await;
                Err(Error::Load(
                    plugin_id.clone(),
                    "initialization timed out".into(),
                ))
            },
        )
        .await?;
        Ok(Self { _store: store })
    }
}

/// Creates the per-plugin WASI sandbox and persistent work mount.
async fn build_state(data_directory: PathBuf, manifest: &PluginManifest) -> Result<HostState> {
    let plugin_directory = data_directory.join(manifest.id.to_string());
    let work_directory = plugin_directory.join("work");
    async_fs::create_dir_all(&work_directory).await?;
    let work_directory = async_fs::canonicalize(work_directory).await?;
    let mut wasi = WasiCtxBuilder::new();
    wasi.initial_cwd(".")
        .allow_tcp(false)
        .allow_udp(false)
        .max_random_size(1024 * 1024);
    wasi.preopened_dir(&work_directory, ".", DirPerms::all(), FilePerms::all())
        .map_err(|error| Error::Host(error.to_string()))?;
    Ok(HostState {
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
