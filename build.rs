use std::{collections::BTreeMap, env, fmt::Write, fs, path::PathBuf};

use heck::ToShoutySnakeCase;
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
                interfaces.insert(resolve.name_world_key(key), name.to_shouty_snake_case());
            }
        }
    }

    let mut source = String::new();
    for (name, constant) in &interfaces {
        writeln!(
            source,
            "/// The exact versioned name of the `{name}` export."
        )
        .unwrap();
        writeln!(source, "pub(crate) const {constant}: &str = {name:?};").unwrap();
    }
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("plugin_interfaces.rs"),
        source,
    )
    .unwrap();
}
