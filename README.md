# Bottles plugin host

Loads WASIp3 components and exposes their account and library capabilities as
Bottles core providers. Applications use this crate to manage plugin packages,
load guest instances, and register the capabilities they want to offer.

## Load and register providers

[`Plugins`] manages files in a catalog. Loading a package returns an untyped
[`Plugin`] handle. Use [`Plugin::cast`] to obtain a [`Plugin<Account>`] or
[`Plugin<Library>`], then register it with core. Registration is owned by the
application; loading alone does not add a provider to core.

```text
use std::sync::Arc;
use bottles_core::Bottles;
use bottles_plugin_host::{Account, Library, Plugins};

async fn register_plugins(bottles: &Bottles) -> wasmtime::Result<()> {
    let plugins = Plugins::new(bottles.directories().plugins())?;
    for manifest in plugins.list()? {
        let plugin = plugins.load(&manifest.id).await?;
        if let Some(account) = plugin.cast::<Account>() {
            bottles.profiles().register_provider(Arc::new(account));
        }
        if let Some(library) = plugin.cast::<Library>() {
            bottles.library().register_provider(Arc::new(library));
        }
    }
    Ok(())
}
```

Typed handles implement [`bottles_core::AccountProvider`] and
[`bottles_core::LibraryProvider`]. They share the loaded guest instance, so state
created during account linking can be used by subsequent library calls. A cast
checks the versioned export name; bindings are constructed when a provider method
is invoked, and missing or incompatible functions fail at that point.

Core's library registry returns items whose launch operations must be polled:

```text
async fn launch_first(bottles: &bottles_core::Bottles) -> bottles_core::error::Result<()> {
    if let Some(item) = bottles.library().list().await?.first() {
        item.launch()?.await?;
    }
    Ok(())
}
```

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

Installation and removal affect files. Loaded instances and registered providers
keep their state until their handles are released. Each [`Plugins::load`] call
creates a separate instance; there is no cache of loaded packages.

## Execution and lifetime

Each loaded instance has one store on a dedicated OS thread with a Tokio runtime
that runs on that thread. Calls are queued to that driver and can run concurrently
when the guest yields. Bindings for the requested capability are constructed for
each call. CPU-bound guest code has no fuel or epoch interruption configured and
can occupy its driver thread without yielding.

The driver stays alive while a call sender exists. Typed handles retain the shared
sender, and a library launch [`bottles_core::Operation`] retains a sender of its
own. When the driver observes the closed channel, it exits and drops in-flight
calls. Dropping the catalog does not stop already loaded instances.

Cancelling or dropping the caller's wait does not withdraw a call already queued
to the driver. If launch cancellation ends the wait, the operation reports
[`bottles_core::error::Error::Cancelled`] while the guest may still complete the
launch. Account interactions remain with
their queued invocation until it completes or the driver drops it. The host does
not recreate the instance after a trap.

Guests inherit standard error and can make outbound HTTP requests. No filesystem
directories are preopened. Account input requests are forwarded to the caller's
[`bottles_core::AccountLinkInteraction`]; storing a returned credential belongs to
core's profile workflow.
