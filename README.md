# Bottles plugin host

Loads WASIp3 components and exposes their account and library capabilities as
typed handles. Applications use this crate to manage plugin packages, load guest
instances, and call the capabilities they want to offer.

## Load and call capabilities

[`Plugins`] manages files in a catalog. Loading a package returns an untyped
[`Plugin`] handle. Use [`Plugin::cast`] to obtain a [`Plugin<Account>`] or
[`Plugin<Library>`], then keep the handles wherever the application needs them.

```text
use bottles_plugin_host::{Library, Plugins};

async fn list_plugin_entries() -> wasmtime::Result<()> {
    let plugins = Plugins::new()?;
    for manifest in plugins.list()? {
        let plugin = plugins.load(&manifest.id).await?;
        if let Some(library) = plugin.cast::<Library>() {
            for entry in library.list_entries().await? {
                println!("{}: {}", manifest.id, entry.title);
            }
        }
    }
    Ok(())
}
```

Typed handles share the loaded guest instance, so state created during account
linking can be used by subsequent library calls. A cast checks the versioned
export name; bindings are constructed when a capability method is invoked, and
missing or incompatible functions fail at that point.

## Package layout

The catalog contains one directory per package:

```text
plugins/
  example/
    plugin.toml
    plugin.wasm
```

`plugin.toml` describes [`Manifest`]:

```text
id = "example"
name = "Example provider"
version = "0.1.0"
```

`plugin.wasm` is a component built with the guest SDK, `bottles-plugin-api`.
[`Plugins::install`] copies these two files from a source directory and overwrites
existing files. [`Plugins::list`] reads each directory's manifest in filesystem
order. [`Plugins::new`] does not create the catalog directory; listing requires it
to exist. Package IDs are joined directly to the catalog path.

Installation and removal affect files. Loaded instances keep their state until their handles are released. Each [`Plugins::load`] call
creates a separate instance. Wasmtime's disk cache reuses compiled components
across loads and launches, with invalidation handled by Wasmtime. The CLI and UI
share the `wasmtime` directory under Bottles' cache directory. Guest instance
state is not cached.

## Execution and lifetime

Each loaded instance has one store on a dedicated OS thread with a Tokio runtime
that runs on that thread. Calls are queued to that driver and can run concurrently
when the guest yields. Bindings for the requested capability are constructed for
each call. CPU-bound guest code has no fuel or epoch interruption configured and
can occupy its driver thread without yielding.

The driver stays alive while a call sender exists. Typed handles retain the shared
sender. When the driver observes the closed channel, it exits and drops in-flight
calls. Dropping the catalog does not stop already loaded instances.

Dropping the caller's future does not withdraw a call already queued to the
driver, so the guest may still complete a launch. Account interactions remain with
their queued invocation until it completes or the driver drops it. The host does
not recreate the instance after a trap.

Guests inherit standard error and can make outbound HTTP requests. No filesystem
directories are preopened. Account input requests are forwarded to the caller's
[`Interaction`]; the host does not store returned credentials.
