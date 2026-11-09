# Bottles plugin host

Loads WASIp3 components and exposes their account and library capabilities as
typed handles. Applications use this crate to manage plugin packages, load guest
instances, and call the capabilities they want to offer.

## Load and call capabilities

[`Plugins`] owns the installed packages and one loaded instance of each.
[`Plugins::reload`] brings the loaded plugins in line with the installed ones, and
[`Plugins::providing`] returns a typed handle, such as [`Plugin<Library>`], for
every loaded plugin that provides that capability. [`Plugins::get`] returns the
handle for one plugin.

```text
use bottles_plugin_host::{Library, Plugins};

async fn list_plugin_entries() -> wasmtime::Result<()> {
    let plugins = Plugins::new()?;
    for (id, error) in plugins.reload().await? {
        eprintln!("{id} failed to load: {error}");
    }
    for library in plugins.providing::<Library>() {
        for entry in library.list_entries().await? {
            println!("{}: {}", library.manifest().id, entry.title);
        }
    }
    Ok(())
}
```

Typed handles share the loaded guest instance, so state created during account
linking can be used by subsequent library calls. Getting a handle checks the versioned
export name; bindings are constructed when a capability method is invoked, and
missing or incompatible functions fail at that point.

## Package layout

The plugins directory holds one directory per package and one per plugin's private data:

```text
plugins/
  installed/
    example/
      plugin.toml
      plugin.wasm
  data/
    example/
```

`plugin.toml` describes [`Manifest`]:

```text
id = "example"
name = "Example provider"
version = "0.1.0"
```

`plugin.wasm` is a component built with the guest SDK, `bottles-plugin-api`.
[`Plugins::install`] copies these two files from a source directory, overwriting
existing files, and [`Plugins::uninstall`] removes the directory. Both forget the
loaded instance, and the next [`Plugins::reload`] applies the change. [`Plugins::list`] reads each directory's manifest in filesystem
order. [`Plugins::new`] creates the catalog directory if it is missing. Package IDs are joined directly to the catalog path.

Each installed plugin has at most one loaded instance, owned by [`Plugins`]; handles
to an instance that was replaced or unloaded keep it alive until they are dropped.
Wasmtime's disk cache reuses compiled components
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
calls. Dropping [`Plugins`] stops an instance once no other handle to it remains.

Dropping the caller's future does not withdraw a call already queued to the
driver, so the guest may still complete a launch. Account interactions remain with
their queued invocation until it completes or the driver drops it. The host does
not recreate the instance after a trap.

Guests inherit standard error and can make outbound HTTP requests. A plugin's
`data/<id>` directory is preopened read-write as `.`, is the only directory it can
see, survives updates, and is removed by [`Plugins::uninstall`]. Account input requests are forwarded to the caller's
[`Interaction`].
