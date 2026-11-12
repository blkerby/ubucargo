//! Exports maintained Ubuntu packaging as a debcargo-conf overlay.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use debian_control::lossless::control::Control;
use toml_edit::{DocumentMut, value};

use crate::{
    cargo::validate_crate_name,
    changelog::prepare_export_changelog,
    config::{PackageConfig, UBUNTU_MAINTAINER, get_package_config_path, read_package_config},
    package::{ManagedFileStatus, inspect_managed_files},
    resolve::{
        ExistingPackage, find_parent_package, get_crate_source_name, parse_exact_version,
        read_existing_package,
    },
    source::{
        filter_maintainer_patch_series,
        tree::{TreeNode, build_tree_plan, scan_tree},
    },
};

/// Exports one local source package into an existing debcargo-conf tree.
#[derive(clap::Args)]
pub struct ExportArgs {
    /// Source-package directory; defaults to the nearest package.
    #[arg(value_name = "PACKAGE_DIR")]
    pub package_dir: Option<PathBuf>,

    /// Root of an existing debcargo-conf repository containing src/.
    #[arg(long, value_name = "DIR")]
    pub debcargo_conf: PathBuf,

    /// Replace the entire existing target crate directory with a fresh export.
    #[arg(long)]
    pub overwrite: bool,
}

/// Exports an overlay directly into the selected crate directory.
pub fn run(args: ExportArgs) -> Result<()> {
    let current = std::env::current_dir()?.canonicalize()?;
    let root = match args.package_dir {
        Some(path) => path
            .canonicalize()
            .context("resolve source-package directory")?,
        None => find_parent_package(&current)
            .context("not inside a source package; supply PACKAGE_DIR")?,
    };
    let config_path = get_package_config_path(&root);
    let config = read_package_config(&config_path, None)?;
    if config.resolved_crate_src_path.is_some() {
        bail!("debian-export requires a registry-backed package; crate_src_path is configured");
    }
    let package = read_existing_package(&root)?;
    let crate_name = &package.crate_selection.crate_name;
    validate_crate_name(crate_name)?;
    let version = parse_exact_version(&package.crate_selection.version)?;
    let source_name = get_crate_source_name(
        crate_name,
        if config.semver_suffix {
            Some(&version)
        } else {
            None
        },
    );
    let destination = resolve_export_destination(&root, &args.debcargo_conf, &source_name)?;
    write_export_overlay(
        &package,
        &config,
        &source_name,
        &destination,
        args.overwrite,
    )?;
    println!("Exported {crate_name} to {}", destination.display());
    println!("Use repackage.sh in debcargo-conf to finish packaging.");
    Ok(())
}

/// Resolves the target crate directory and checks its separation from the input.
fn resolve_export_destination(
    root: &Path,
    repository: &Path,
    source_name: &str,
) -> Result<PathBuf> {
    const INVALID_REPOSITORY: &str =
        "--debcargo-conf must name an existing repository root containing src/";
    let crate_directory = source_name.strip_prefix("rust-").unwrap();
    let sources = repository
        .join("src")
        .canonicalize()
        .context(INVALID_REPOSITORY)?;
    if !sources.is_dir() {
        bail!(INVALID_REPOSITORY);
    }
    let destination = sources.join(crate_directory);
    if root.starts_with(&destination) || destination.starts_with(root) {
        bail!("source package and export must be separate, non-nested directory trees");
    }
    Ok(destination)
}

/// Selects unmodified generated files to omit from an overlay, warning about lost deletions.
fn collect_export_omissions(debian: &Path) -> Result<BTreeSet<PathBuf>> {
    let statuses = inspect_managed_files(debian)?
        .context("missing ownership manifest; run `ubucargo package` before exporting")?;
    let mut omissions = BTreeSet::new();
    for (path, status) in statuses {
        match status {
            ManagedFileStatus::Unchanged => {
                omissions.insert(path);
            }
            ManagedFileStatus::Modified => {}
            ManagedFileStatus::Deleted => eprintln!(
                "warning: deletion of {} cannot be represented in a debcargo overlay; debcargo may regenerate it",
                path.display()
            ),
            ManagedFileStatus::Unknown => bail!(
                "missing ownership baseline for {}; run `ubucargo package` before exporting",
                path.display()
            ),
        }
    }
    Ok(omissions)
}

/// Selects maintained packaging files and their parent directories for the overlay.
fn collect_export_tree(debian: &Path) -> Result<BTreeMap<PathBuf, TreeNode>> {
    let omissions = collect_export_omissions(debian)?;
    let mut tree = scan_tree(debian, None)?;
    for path in ["debcargo.toml", "changelog"] {
        if !matches!(tree.get(Path::new(path)), Some(TreeNode::File(_))) {
            bail!("debian/{path} must be a regular file");
        }
    }
    let format_path = Path::new("source/format");
    if matches!(tree.get(format_path), Some(TreeNode::File(_)))
        && fs::read(debian.join(format_path))?.trim_ascii() == b"3.0 (quilt)"
    {
        tree.remove(format_path);
    }
    tree.retain(|path, _| {
        path != Path::new("ubucargo-state.json")
            && path != Path::new("changelog")
            && !path.starts_with("patches/auto")
            && !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".debcargo.hint"))
            && !omissions.contains(&PathBuf::from("debian").join(path))
    });
    Ok(tree)
}

/// Recreates the destination as requested and writes the Debian overlay directly to it.
fn write_export_overlay(
    package: &ExistingPackage,
    config: &PackageConfig,
    source_name: &str,
    destination: &Path,
    overwrite: bool,
) -> Result<()> {
    let replacing = check_export_destination(destination, overwrite)?;
    let tree = collect_export_tree(&package.root.join("debian"))?;
    let mut document: DocumentMut = config.original_contents.parse()?;
    if replacing {
        fs::remove_dir_all(destination)
            .with_context(|| format!("remove previous export {}", destination.display()))?;
    }
    fs::create_dir(destination).with_context(|| format!("create {}", destination.display()))?;
    let exported_debian = destination.join("debian");
    fs::create_dir(&exported_debian)?;
    build_tree_plan(&BTreeMap::new(), &tree).apply(&exported_debian)?;

    if document.get("maintainer").and_then(|item| item.as_str()) == Some(UBUNTU_MAINTAINER) {
        document.remove("maintainer");
    }
    document["overlay"] = value(".");
    fs::write(exported_debian.join("debcargo.toml"), document.to_string())?;

    if tree.contains_key(Path::new("control")) {
        restore_debian_control(
            &exported_debian.join("control"),
            &document,
            source_name.strip_prefix("rust-").unwrap(),
        )?;
    }
    let series_path = exported_debian.join("patches/series");
    if tree.contains_key(Path::new("patches/series")) {
        if !matches!(
            tree.get(Path::new("patches/series")),
            Some(TreeNode::File(_))
        ) {
            bail!("debian/patches/series must be a regular file");
        }
        let series = filter_maintainer_patch_series(&fs::read_to_string(&series_path)?);
        if series.trim().is_empty() {
            fs::remove_file(&series_path)?;
        } else {
            fs::write(&series_path, series)?;
        }
    }
    prepare_export_changelog(package, source_name, &exported_debian.join("changelog"))?;

    // Remove empty directories (standing from children and working outward to parents).
    // They can't be represented in debcargo-conf's git repository anyway, and it helps
    // give a cleaner export.
    for (path, node) in tree.iter().rev() {
        if matches!(node, TreeNode::Directory) {
            let directory = exported_debian.join(path);
            if fs::read_dir(&directory)?.next().transpose()?.is_none() {
                fs::remove_dir(&directory)?;
            }
        }
    }
    Ok(())
}

/// Restores Debian maintainer and repository fields while preserving other control overrides.
fn restore_debian_control(
    path: &Path,
    document: &DocumentMut,
    crate_directory: &str,
) -> Result<()> {
    let control = Control::from_file(path).context("parse exported debian/control")?;
    let mut source = control
        .source()
        .context("exported control has no source paragraph")?;
    source.set_maintainer(
        document
            .get("maintainer")
            .and_then(|item| item.as_str())
            .unwrap_or("Debian Rust Maintainers <pkg-rust-maintainers@alioth-lists.debian.net>"),
    );
    source.as_mut_deb822().remove("XSBC-Original-Maintainer");
    source.as_mut_deb822().remove("Original-Maintainer");
    let source_config = document.get("source");
    let vcs_git = source_config
        .and_then(|source| source.get("vcs_git"))
        .and_then(|item| item.as_str());
    let vcs_browser = source_config
        .and_then(|source| source.get("vcs_browser"))
        .and_then(|item| item.as_str());
    source.set_vcs_git(vcs_git.unwrap_or(&format!(
        "https://salsa.debian.org/rust-team/debcargo-conf.git [src/{crate_directory}]"
    )));
    source.set_vcs_browser(Some(vcs_browser.unwrap_or(&format!(
        "https://salsa.debian.org/rust-team/debcargo-conf/tree/master/src/{crate_directory}"
    ))));
    fs::write(path, control.to_string()).context("write exported debian/control")
}

/// Rejects unsafe targets and existing exports without explicit replacement, returning existence.
fn check_export_destination(destination: &Path, overwrite: bool) -> Result<bool> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) => {
            if !overwrite {
                bail!(
                    "export directory already exists: {}\nUse --overwrite to replace it with a fresh export.",
                    destination.display()
                );
            }
            if !metadata.is_dir() {
                bail!(
                    "export destination must be a directory: {}",
                    destination.display()
                );
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect {}", destination.display())),
    }
}
