mod bindings;
mod error;
mod manager;
mod manifest;
mod package;
mod runtime;

pub use error::{Error, Result};
pub use manager::{PluginInfo, PluginManager, PluginStatus};
pub use manifest::PluginManifest;

pub const API_VERSION: &str = bottles_plugin_api::API_VERSION;
