use std::{collections::BTreeMap, env, fmt::Write, fs, path::PathBuf};

use heck::ToUpperCamelCase;
use wit_parser::{Resolve, WorldItem};

fn main() {
    let wit = "../next-plugin-api/wit";
    println!("cargo:rerun-if-changed={wit}");
    let mut resolve = Resolve::default();
    let (package, _) = resolve.push_dir(wit).unwrap();
    let mut interfaces = BTreeMap::new();
    for world in resolve.packages[package].worlds.values() {
        for (key, item) in &resolve.worlds[*world].exports {
            if let WorldItem::Interface { id, .. } = item {
                let name = resolve.interfaces[*id].name.as_ref().unwrap();
                interfaces.insert(resolve.name_world_key(key), name.to_upper_camel_case());
            }
        }
    }

    let mut source = String::from(
        r#"/// Names the provider interfaces exported by the SDK's WIT worlds.
///
/// Used by [`crate::Capability::INTERFACE`] to select the export required
/// by a typed [`crate::Plugin`] handle. [`Self::as_str`] returns the exact
/// versioned name used in component exports.
///
/// # Examples
///
/// ```text
/// use bottles_plugin_host::{Capability, Library};
///
/// let name = <Library as Capability>::INTERFACE.as_str();
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginInterface {
"#,
    );
    for (name, variant) in &interfaces {
        writeln!(source, "    /// Identifies the `{name}` export.").unwrap();
        writeln!(source, "    {variant},").unwrap();
    }
    source.push_str(
        r#"}
impl PluginInterface {
    /// Returns the exact versioned WIT interface name.
    ///
    /// # Examples
    ///
    /// ```text
    /// use bottles_plugin_host::PluginInterface;
    ///
    /// let name = PluginInterface::LibraryProvider.as_str();
    /// // name is "bottles:plugin/library-provider@0.1.0"
    /// ```
    pub const fn as_str(self) -> &'static str {
        match self {
"#,
    );
    for (name, variant) in &interfaces {
        writeln!(source, "            Self::{variant} => {name:?},").unwrap();
    }
    source.push_str("        }\n    }\n}\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("plugin_interfaces.rs"),
        source,
    )
    .unwrap();
}
