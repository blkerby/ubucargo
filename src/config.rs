//! Locates, reads, and writes debcargo.toml configuration for packages and staging.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, value};

const UBUNTU_MAINTAINER: &str = "Ubuntu Developers <ubuntu-devel-discuss@lists.ubuntu.com>";

/// Snapshot of the configuration intended for the final package and its effective values.
pub struct PackageConfig {
    /// Complete configuration before staging overrides, preserving relative paths and defaults.
    pub original_contents: String,
    /// Whether the Debian source name includes the crate's semver line; defaults to false.
    pub semver_suffix: bool,
    /// Explicit repack suffix, or "ds" when only `excludes` is set, or none.
    pub effective_repack_suffix: Option<String>,
    /// Canonical absolute local source path, or none for crates.io.
    /// Relative paths in `original_contents` refer to the final package's `debian` directory.
    pub resolved_crate_src_path: Option<PathBuf>,
}

/// Returns the persisted configuration path within a source package.
pub fn get_package_config_path(package_root: &Path) -> PathBuf {
    package_root.join("debian/debcargo.toml")
}

/// Returns the temporary configuration path used by debcargo commands.
pub fn get_staged_config_path(stage: &Path) -> PathBuf {
    stage.join("debcargo.toml")
}

/// Writes the original configuration text into a source package.
/// The package may still be staged; relative paths already refer to its final location.
pub fn write_package_config(config: &PackageConfig, package_root: &Path) -> Result<()> {
    let path = get_package_config_path(package_root);
    fs::write(&path, &config.original_contents).with_context(|| format!("write {}", path.display()))
}

/// Reports whether a directory contains debcargo configuration for package generation.
pub fn has_debcargo_config(package_root: &Path) -> bool {
    get_package_config_path(package_root).is_file()
}

/// Reads configuration, using an explicit local crate in preference to its saved source path.
pub fn read_package_config(
    package_root: &Path,
    local_crate: Option<&Path>,
) -> Result<PackageConfig> {
    let path = get_package_config_path(package_root);
    let contents = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    parse_package_config(&contents, &package_root.join("debian"), local_crate)
        .with_context(|| format!("read configuration {}", path.display()))
}

/// Creates the persisted Ubuntu configuration used for a new package.
pub fn get_new_package_config() -> Result<PackageConfig> {
    let mut document = DocumentMut::new();
    document["maintainer"] = value(UBUNTU_MAINTAINER);
    parse_package_config(&document.to_string(), Path::new(""), None)
}

/// Configures a local crate, using a relative source path when a destination is given.
pub fn get_new_local_package_config(
    crate_root: &Path,
    package_root: Option<&Path>,
) -> Result<PackageConfig> {
    let mut document = DocumentMut::new();
    document["maintainer"] = value(UBUNTU_MAINTAINER);
    let config_dir = package_root.map(|root| root.join("debian"));
    parse_package_config(
        &document.to_string(),
        config_dir.as_deref().unwrap_or(Path::new("")),
        Some(crate_root),
    )
}

/// Parses configuration, resolving paths relative to `config_dir`.
/// An explicit local source replaces the saved path and is persisted relative to that directory.
fn parse_package_config(
    contents: &str,
    config_dir: &Path,
    local_crate: Option<&Path>,
) -> Result<PackageConfig> {
    let mut config: DocumentMut = contents.parse().context("parse debcargo configuration")?;
    if let Some(overlay) = config.get("overlay")
        && overlay.as_str() != Some(".")
    {
        bail!("overlay must be omitted or \".\"");
    }
    let resolved_crate_src_path = if let Some(local_crate) = local_crate {
        Some(
            local_crate
                .canonicalize()
                .with_context(|| format!("resolve local crate {}", local_crate.display()))?,
        )
    } else if let Some(item) = config.get("crate_src_path") {
        let path = config_dir.join(item.as_str().context("crate_src_path must be a string")?);
        Some(
            path.canonicalize()
                .with_context(|| format!("resolve crate_src_path {}", path.display()))?,
        )
    } else {
        None
    };
    if local_crate.is_some()
        && let Some(source) = &resolved_crate_src_path
    {
        let path = if config_dir.as_os_str().is_empty() {
            source.clone()
        } else {
            pathdiff::diff_paths(source, config_dir)
                .context("make crate_src_path relative to package destination")?
        };
        let path = require_utf8_path(&path)?;
        if config.get("crate_src_path").and_then(|item| item.as_str()) != Some(path) {
            let mut item = value(path);
            if let Some(previous) = config
                .get("crate_src_path")
                .and_then(|item| item.as_value())
            {
                *item.as_value_mut().unwrap().decor_mut() = previous.decor().clone();
            }
            config["crate_src_path"] = item;
        }
    }
    let semver_suffix = config
        .get("semver_suffix")
        .and_then(|item| item.as_bool())
        .unwrap_or(false);
    let effective_repack_suffix = if let Some(item) = config.get("repack_suffix") {
        Some(
            item.as_str()
                .context("repack_suffix must be a string")?
                .to_owned(),
        )
    } else if config.get("excludes").is_some() {
        Some("ds".to_owned())
    } else {
        None
    };
    Ok(PackageConfig {
        original_contents: if local_crate.is_some() {
            config.to_string()
        } else {
            contents.to_owned()
        },
        semver_suffix,
        effective_repack_suffix,
        resolved_crate_src_path,
    })
}

/// Writes staged configuration with resolved local source and temporary overlay paths.
/// Absolute source paths retain their meaning when the configuration moves into staging.
pub fn write_staged_config(config: &PackageConfig, stage: &Path) -> Result<()> {
    let mut document: DocumentMut = config.original_contents.parse()?;
    if let Some(crate_src_path) = &config.resolved_crate_src_path {
        document["crate_src_path"] = value(require_utf8_path(crate_src_path)?);
    }
    document["overlay"] = value(require_utf8_path(&stage.join("overlay"))?);
    fs::write(get_staged_config_path(stage), document.to_string())?;
    Ok(())
}

/// Returns a path as UTF-8 for insertion into TOML.
fn require_utf8_path(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

/// Rebases a relative local-crate path for a package's new location.
pub fn relocate_package_config(config: &mut PackageConfig, package_root: &Path) -> Result<()> {
    let Some(crate_root) = &config.resolved_crate_src_path else {
        return Ok(());
    };
    let mut document: DocumentMut = config.original_contents.parse()?;
    let source = document["crate_src_path"]
        .as_str()
        .context("crate_src_path must be a string")?;
    if Path::new(source).is_relative() {
        let relative = pathdiff::diff_paths(crate_root, package_root.join("debian"))
            .context("make crate_src_path relative to package destination")?;
        document["crate_src_path"] = value(require_utf8_path(&relative)?);
        config.original_contents = document.to_string();
    }
    Ok(())
}
