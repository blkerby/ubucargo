//! Locates, reads, and writes debcargo.toml configuration for packages and staging.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, value};

const UBUNTU_MAINTAINER: &str = "Ubuntu Developers <ubuntu-devel-discuss@lists.ubuntu.com>";

/// Debcargo configuration values that affect package identity.
pub struct PackageConfig {
    /// Complete in-tree configuration text.
    pub contents: String,
    /// Whether the Debian source name includes the crate's semver line.
    pub semver_suffix: bool,
    /// Effective repack suffix, e.g. "ds".
    pub repack_suffix: Option<String>,
    /// Resolved local crate source, or none for crates.io.
    pub crate_src_path: Option<PathBuf>,
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
pub fn write_package_config(config: &PackageConfig, package_root: &Path) -> Result<()> {
    let path = get_package_config_path(package_root);
    fs::write(&path, &config.contents).with_context(|| format!("write {}", path.display()))
}

/// Reports whether a directory contains Ubucargo's source-package marker.
pub fn has_debcargo_config(package_root: &Path) -> bool {
    get_package_config_path(package_root).is_file()
}

/// Reads and validates the in-tree debcargo configuration.
pub fn read_package_config(package_root: &Path) -> Result<PackageConfig> {
    let path = get_package_config_path(package_root);
    let contents = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut config =
        read_package_config_text(&contents).with_context(|| format!("parse {}", path.display()))?;
    if let Some(crate_src_path) = &config.crate_src_path {
        let config_dir = path
            .parent()
            .context("debcargo.toml has no parent directory")?;
        config.crate_src_path = Some(
            config_dir
                .join(crate_src_path)
                .canonicalize()
                .with_context(|| format!("resolve crate_src_path from {}", path.display()))?,
        );
    }
    Ok(config)
}

/// Creates the persisted Ubuntu configuration used for a new package.
pub fn read_new_package_config() -> Result<PackageConfig> {
    let mut document = DocumentMut::new();
    document["maintainer"] = value(UBUNTU_MAINTAINER);
    read_package_config_text(&document.to_string())
}

/// Creates the persisted configuration for a new package built from a local crate.
pub fn read_new_local_package_config(
    crate_root: &Path,
    package_root: &Path,
) -> Result<PackageConfig> {
    let path = get_package_config_path(package_root);
    let config_dir = path
        .parent()
        .context("debcargo.toml has no parent directory")?;
    let relative = pathdiff::diff_paths(crate_root, config_dir).with_context(|| {
        format!(
            "cannot express {} relative to {}",
            crate_root.display(),
            config_dir.display()
        )
    })?;
    let mut config = read_new_package_config()?;
    let mut document: DocumentMut = config.contents.parse()?;
    document["crate_src_path"] = value(require_utf8_path(&relative)?);
    config.contents = document.to_string();
    config.crate_src_path = Some(crate_root.to_path_buf());
    Ok(config)
}

/// Reads the package-identity subset of a debcargo configuration.
fn read_package_config_text(contents: &str) -> Result<PackageConfig> {
    let config: DocumentMut = contents.parse().context("parse debcargo configuration")?;
    if let Some(overlay) = config.get("overlay")
        && overlay.as_str() != Some(".")
    {
        bail!("overlay must be omitted or \".\"");
    }
    let crate_src_path = config
        .get("crate_src_path")
        .map(|item| {
            item.as_str()
                .context("crate_src_path must be a string")
                .map(PathBuf::from)
        })
        .transpose()?;
    let semver_suffix = config
        .get("semver_suffix")
        .and_then(|item| item.as_bool())
        .unwrap_or(false);
    let repack_suffix = if let Some(item) = config.get("repack_suffix") {
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
        contents: contents.to_owned(),
        semver_suffix,
        repack_suffix,
        crate_src_path,
    })
}

/// Writes staged configuration with resolved local source and temporary overlay paths.
pub fn write_staged_config(config: &PackageConfig, stage: &Path) -> Result<()> {
    let mut document: DocumentMut = config.contents.parse()?;
    if let Some(crate_src_path) = &config.crate_src_path {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Preserves the new-package maintainer through configuration reload and staging.
    fn preserves_new_package_maintainer() {
        let stage = tempfile::tempdir().unwrap();
        let crate_root = stage.path().join("crate");
        let package_root = stage.path().join("output");
        fs::create_dir(&crate_root).unwrap();
        fs::create_dir_all(package_root.join("debian")).unwrap();
        for config in [
            read_new_package_config().unwrap(),
            read_new_local_package_config(&crate_root, &package_root).unwrap(),
        ] {
            write_staged_config(&config, stage.path()).unwrap();
            let initial = fs::read_to_string(stage.path().join("debcargo.toml")).unwrap();
            write_package_config(&config, &package_root).unwrap();
            let reloaded = read_package_config(&package_root).unwrap();
            let document: DocumentMut = reloaded.contents.parse().unwrap();
            assert_eq!(document["maintainer"].as_str(), Some(UBUNTU_MAINTAINER));
            assert!(!document.contains_key("overlay"));

            write_staged_config(&reloaded, stage.path()).unwrap();
            assert_eq!(
                fs::read_to_string(stage.path().join("debcargo.toml")).unwrap(),
                initial
            );
        }
    }

    #[test]
    /// Leaves existing maintainer settings and debcargo's implicit default unchanged.
    fn preserves_existing_maintainer_config() {
        let stage = tempfile::tempdir().unwrap();
        fs::create_dir(stage.path().join("debian")).unwrap();
        let path = get_package_config_path(stage.path());
        for contents in [
            "# Use debcargo's default maintainer.\n",
            "maintainer = \"Debian Rust Maintainers <pkg-rust-maintainers@alioth-lists.debian.net>\"\n",
            "maintainer = \"Example Developer <example@ubuntu.com>\"\n",
        ] {
            fs::write(&path, contents).unwrap();
            let config = read_package_config(stage.path()).unwrap();
            write_staged_config(&config, stage.path()).unwrap();
            let mut staged: DocumentMut = fs::read_to_string(stage.path().join("debcargo.toml"))
                .unwrap()
                .parse()
                .unwrap();
            staged.remove("overlay");
            assert_eq!(staged.to_string(), contents);
            assert_eq!(fs::read_to_string(&path).unwrap(), contents);
        }
    }

    #[test]
    /// Persists a relative local source while staging its resolved absolute path.
    fn configures_local_crate_source() {
        let parent = tempfile::tempdir().unwrap();
        let crate_root = parent.path().join("example");
        let package_root = parent.path().join("rust-example");
        fs::create_dir(&crate_root).unwrap();
        assert!(!has_debcargo_config(&package_root));
        assert!(read_package_config(&package_root).is_err());
        let config = read_new_local_package_config(&crate_root, &package_root).unwrap();
        assert!(
            config
                .contents
                .contains("crate_src_path = \"../../example\"")
        );

        fs::create_dir_all(package_root.join("debian")).unwrap();
        write_package_config(&config, &package_root).unwrap();
        assert!(has_debcargo_config(&package_root));
        assert_eq!(
            read_package_config(&package_root)
                .unwrap()
                .crate_src_path
                .as_deref(),
            Some(crate_root.as_path())
        );

        let stage = tempfile::tempdir().unwrap();
        write_staged_config(&config, stage.path()).unwrap();
        let staged: DocumentMut = fs::read_to_string(stage.path().join("debcargo.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(staged["crate_src_path"].as_str(), crate_root.to_str());
    }

    #[test]
    /// Selects debcargo's configured suffix or default without altering staged configuration.
    fn selects_repack_suffix() {
        let stage = tempfile::tempdir().unwrap();
        for (contents, expected) in [
            ("", None),
            ("excludes = [\"benches/**\"]\n", Some("ds")),
            ("repack_suffix = \"custom\"\n", Some("custom")),
            (
                "excludes = [\"benches/**\"]\nrepack_suffix = \"custom\"\n",
                Some("custom"),
            ),
        ] {
            let config = read_package_config_text(contents).unwrap();
            assert_eq!(config.repack_suffix.as_deref(), expected);
            write_staged_config(&config, stage.path()).unwrap();
            let mut staged: DocumentMut = fs::read_to_string(get_staged_config_path(stage.path()))
                .unwrap()
                .parse()
                .unwrap();
            staged.remove("overlay");
            assert_eq!(staged.to_string(), contents);
        }
    }
}
