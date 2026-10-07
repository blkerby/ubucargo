//! Parses, validates, and prepares Debian changelog entries.

use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, bail};

use crate::{resolve::ResolvedPackage, util::run_command};
use debian_changelog::ChangeLog;

/// Parsed fields from the first Debian changelog entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopChangelog {
    /// Debian source package name.
    pub source: String,
    /// Complete Debian source version.
    pub version: String,
    /// Debian upstream version without epoch or Debian revision.
    pub upstream: String,
    /// Distribution named by the top entry.
    pub distribution: String,
}

/// Parses only the first Debian changelog header.
pub fn read_top_changelog(path: &Path) -> Result<TopChangelog> {
    let changelog =
        ChangeLog::read_path(path).with_context(|| format!("read {}", path.display()))?;
    parse_top_changelog(&changelog).with_context(|| format!("parse {}", path.display()))
}

/// Requires the top changelog upstream to describe the current root Cargo release.
pub fn validate_top_changelog(
    top: &TopChangelog,
    cargo_version: &str,
    cargo_upstream: &str,
) -> Result<()> {
    if top.upstream != cargo_upstream
        && !top
            .upstream
            .strip_prefix(cargo_upstream)
            .is_some_and(|suffix| suffix.starts_with('+') && suffix.len() > 1)
    {
        bail!(
            "top changelog upstream {} does not describe root Cargo version {}",
            top.upstream,
            cargo_version
        );
    }
    Ok(())
}

/// Prepares the resolved package's changelog with `dch`, then normalizes its top entry.
pub fn prepare_changelog(package: &ResolvedPackage, staged_path: &Path) -> Result<()> {
    if let Some(existing) = &package.existing {
        let old_path = existing.root.join("debian/changelog");
        fs::copy(&old_path, staged_path)
            .with_context(|| format!("copy {} to {}", old_path.display(), staged_path.display()))?;
    }
    let mut command = Command::new("dch");
    command
        .arg("--no-conf")
        .arg("--vendor")
        .arg("Ubuntu")
        .arg("--changelog")
        .arg(staged_path)
        .arg("--check-dirname-level")
        .arg("0")
        .arg("--distribution")
        .arg("UNRELEASED")
        .arg("--force-distribution");
    let source_name = package.source_name.as_str();
    let upstream = package.upstream.as_str();
    let initial_version = format!("{upstream}-0ubuntu1");
    let old_top = package
        .existing
        .as_ref()
        .map(|existing| &existing.top_changelog);
    match old_top {
        None => {
            command
                .arg("--create")
                .arg("--package")
                .arg(source_name)
                .arg("--newversion")
                .arg(&initial_version);
        }
        Some(old) if old.upstream == upstream => {
            command.arg(if old.distribution == "UNRELEASED" {
                "--append"
            } else {
                "--increment"
            });
        }
        Some(_) => {
            command.arg("--newversion").arg(&initial_version);
        }
    }
    let source_kind = if package.config.resolved_crate_src_path.is_some() {
        "local source"
    } else {
        "crates.io"
    };
    let provenance = format!(
        "Package {} {} from {source_kind}.\n  Generated with debcargo {} and ubucargo {}.",
        package.crate_selection.crate_name,
        package.crate_selection.version,
        package.debcargo_version,
        env!("CARGO_PKG_VERSION")
    );
    run_command(command.arg(&provenance), "dch")?;

    let mut changelog = ChangeLog::read_path(staged_path).context("read prepared changelog")?;
    if let Some(old) = old_top
        && old.source != source_name
    {
        let mut entry = changelog.iter().next().context("changelog is empty")?;
        entry.set_package(source_name.to_owned());
        entry.prepend_change_line(&format!(
            "* Rename source package from {} to {source_name}.",
            old.source
        ));
    }
    normalize_top_entry(&mut changelog, &provenance)?;
    changelog
        .write_to_path(staged_path)
        .context("write prepared changelog")?;
    let top = parse_top_changelog(&changelog)?;
    if top.source != source_name || top.upstream != upstream {
        bail!(
            "prepared changelog identifies {} {}, expected {source_name} {upstream}",
            top.source,
            top.upstream
        );
    }
    Ok(())
}

/// Extracts the top entry fields from a parsed changelog.
fn parse_top_changelog(changelog: &ChangeLog) -> Result<TopChangelog> {
    let entry = changelog.iter().next().context("changelog is empty")?;
    let source = entry.package().context("changelog entry has no source")?;
    let version = entry
        .try_version()
        .transpose()
        .context("parse changelog version")?
        .context("changelog entry has no version")?;
    let distribution = entry
        .distributions()
        .context("changelog entry has no distribution")?
        .join(" ");
    if distribution.trim().is_empty() {
        bail!("changelog entry has no distribution");
    }
    Ok(TopChangelog {
        source,
        version: version.to_string(),
        upstream: version.upstream_version,
        distribution,
    })
}

/// Normalizes the top entry distribution and provenance bullet.
fn normalize_top_entry(changelog: &mut ChangeLog, provenance: &str) -> Result<()> {
    let entry = changelog.iter().next().context("changelog is empty")?;
    let top_version = entry
        .try_version()
        .transpose()
        .context("parse top changelog version")?
        .context("top changelog entry has no version")?;
    let provenance = format!("* {provenance}");
    let mut provenance_bullet = None;
    for change in debian_changelog::iter_changes_by_author(changelog) {
        if change.package() != entry.package()
            || change.try_version().transpose()? != Some(top_version.clone())
        {
            break;
        }
        for bullet in change.split_into_bullets() {
            if is_provenance(bullet.lines())
                && let Some(previous) = provenance_bullet.replace(bullet)
            {
                previous.remove();
            }
        }
    }
    if let Some(bullet) = provenance_bullet {
        bullet.replace_with(provenance.lines().collect());
    } else {
        for line in provenance.lines().rev() {
            entry.prepend_change_line(line);
        }
    }
    Ok(())
}

/// Checks whether changelog lines identify an Ubucargo/debcargo provenance bullet.
fn is_provenance(lines: Vec<String>) -> bool {
    let Some(first) = lines.first() else {
        return false;
    };
    first.starts_with("* Package ")
        && lines
            .iter()
            .any(|line| line.contains("from crates.io") || line.contains("from local source"))
        && lines.iter().any(|line| line.contains("debcargo"))
}
