use std::path::Path;

use async_process::Command;
use uuid::Uuid;
use wasmparser::{Parser, Payload};

use crate::{
    Error, Result,
    manifest::{MANIFEST_FILE, PluginManifest},
};

const API_SECTION: &str = "bottles:api-version";
const COMPONENT_FILE: &str = "plugin.wasm";
const RUST_WASM_TARGET: &str = "wasm32-wasip2";

/// A manifest and component that passed structural and exact API-version checks.
pub struct ValidatedPackage {
    pub manifest: PluginManifest,
    pub component: Vec<u8>,
}

/// Builds the root Cargo package for `wasm32-wasip2` and writes the validated
/// component to `<directory>/plugin.wasm`.
///
/// Cargo output remains beneath `<directory>/target`. A missing Rust target is
/// installed through rustup before the build starts.
pub async fn build_source(directory: &Path) -> Result<ValidatedPackage> {
    let manifest = PluginManifest::load(directory).await?;
    let cargo_manifest = async_fs::read_to_string(directory.join("Cargo.toml")).await?;
    let target_name = source_target_name(&cargo_manifest)?;
    install_rust_target_if_needed(directory).await?;
    let output = Command::new("cargo")
        .args(["build", "--release", "--target", RUST_WASM_TARGET])
        .arg("--target-dir")
        .arg(directory.join("target"))
        .current_dir(directory)
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        return Err(Error::Host(format!(
            "cargo build failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let component_path = directory
        .join("target")
        .join(RUST_WASM_TARGET)
        .join("release")
        .join(target_name)
        .with_extension("wasm");
    let component = async_fs::read(component_path).await?;
    validate_component(&manifest, &component)?;
    async_fs::write(directory.join(COMPONENT_FILE), &component).await?;
    Ok(ValidatedPackage {
        manifest,
        component,
    })
}

async fn install_rust_target_if_needed(directory: &Path) -> Result<()> {
    let output = Command::new("rustc")
        .args(["--print", "target-libdir", "--target", RUST_WASM_TARGET])
        .current_dir(directory)
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        return Err(Error::Host(format!(
            "failed to locate the {RUST_WASM_TARGET} target: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    if Path::new(String::from_utf8_lossy(&output.stdout).trim()).exists() {
        return Ok(());
    }

    let output = Command::new("rustup")
        .args(["target", "add", RUST_WASM_TARGET])
        .current_dir(directory)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| Error::Host(format!("failed to run rustup: {error}")))?;
    if !output.status.success() {
        return Err(Error::Host(format!(
            "failed to install the {RUST_WASM_TARGET} target: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

/// Loads an unpacked runtime package and verifies compatibility before Wasmtime
/// receives its component bytes.
pub async fn validate_package(directory: &Path) -> Result<ValidatedPackage> {
    let manifest = PluginManifest::load(directory).await?;
    let component = async_fs::read(directory.join(COMPONENT_FILE)).await?;
    validate_component(&manifest, &component)?;
    Ok(ValidatedPackage {
        manifest,
        component,
    })
}

/// Parses the complete component and requires its embedded API version to match
/// both the manifest and this host.
fn validate_component(manifest: &PluginManifest, bytes: &[u8]) -> Result<()> {
    let mut found = None;
    for payload in Parser::new(0).parse_all(bytes) {
        let payload = payload.map_err(|error| Error::InvalidComponent(error.to_string()))?;
        if let Payload::CustomSection(section) = payload
            && section.name() == API_SECTION
        {
            found = Some(parse_api_version(section.data())?);
        }
    }
    let actual = found
        .ok_or_else(|| Error::InvalidComponent(format!("missing {API_SECTION} custom section")))?;
    if actual != manifest.api_version {
        return Err(Error::ApiVersion {
            actual: actual.to_string(),
            expected: manifest.api_version.to_string(),
        });
    }
    let supported = semver::Version::parse(bottles_plugin_api::API_VERSION)
        .expect("bottles-plugin-api package version is valid semver");
    if actual != supported {
        return Err(Error::ApiVersion {
            actual: actual.to_string(),
            expected: supported.to_string(),
        });
    }
    Ok(())
}

/// Materializes a package in a new directory, removing partial output when a
/// write fails.
async fn write_package(package: &ValidatedPackage, destination: &Path) -> Result<()> {
    if destination.exists() {
        return Err(Error::Host(format!(
            "package destination {} already exists",
            destination.display()
        )));
    }
    async_fs::create_dir_all(destination).await?;
    let result = async {
        let manifest =
            toml::to_string(&package.manifest).map_err(|error| Error::Host(error.to_string()))?;
        async_fs::write(destination.join(MANIFEST_FILE), manifest).await?;
        async_fs::write(destination.join(COMPONENT_FILE), &package.component).await?;
        Result::<()>::Ok(())
    }
    .await;
    if result.is_err() {
        let _ = async_fs::remove_dir_all(destination).await;
    }
    result
}

/// Replaces the installed package through a staging directory on the same
/// filesystem.
///
/// The existing package is removed before the staging rename. A rename failure
/// therefore leaves the plugin uninstalled rather than restoring the old files.
pub async fn activate_package(
    package: &ValidatedPackage,
    installed_directory: &Path,
    staging_directory: &Path,
) -> Result<()> {
    async_fs::create_dir_all(installed_directory).await?;
    async_fs::create_dir_all(staging_directory).await?;
    let installed_directory = async_fs::canonicalize(installed_directory).await?;
    let staging_directory = async_fs::canonicalize(staging_directory).await?;
    let destination = installed_directory.join(package.manifest.id.to_string());
    let staging = staging_directory.join(Uuid::new_v4().to_string());
    write_package(package, &staging).await?;
    match async_fs::remove_dir_all(&destination).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if let Err(error) = async_fs::rename(&staging, &destination).await {
        let _ = async_fs::remove_dir_all(&staging).await;
        return Err(error.into());
    }
    Ok(())
}

/// Decodes the custom-section wire format: three big-endian `u16` values.
fn parse_api_version(bytes: &[u8]) -> Result<semver::Version> {
    let [major_hi, major_lo, minor_hi, minor_lo, patch_hi, patch_lo] = bytes else {
        return Err(Error::InvalidComponent(format!(
            "{API_SECTION} must contain three big-endian u16 values"
        )));
    };
    Ok(semver::Version::new(
        u16::from_be_bytes([*major_hi, *major_lo]).into(),
        u16::from_be_bytes([*minor_hi, *minor_lo]).into(),
        u16::from_be_bytes([*patch_hi, *patch_lo]).into(),
    ))
}

/// Maps a Cargo package name to Cargo's Wasm artifact stem.
fn source_target_name(source: &str) -> Result<String> {
    let manifest: toml::Value =
        toml::from_str(source).map_err(|error| Error::Host(error.to_string()))?;
    manifest
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .map(|name| name.replace('-', "_"))
        .ok_or_else(|| Error::Host("Cargo.toml has no package name".into()))
}
