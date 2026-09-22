//! Parses, validates, and prepares Debian changelog entries.

use std::{fs, path::Path, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};

use crate::command::run_command;
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

/// Prepares changelog with `dch`, then normalizes its top entry.
pub fn prepare_changelog(
    old_path: Option<PathBuf>,
    staged_path: &Path,
    old_top: Option<&TopChangelog>,
    source_name: &str,
    upstream: &str,
    provenance: &str,
) -> Result<()> {
    if let Some(old_path) = old_path {
        fs::copy(&old_path, staged_path)
            .with_context(|| format!("copy {} to {}", old_path.display(), staged_path.display()))?;
    }
    let initial_version = format!("{upstream}-0ubuntu1");
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
    run_command(command.arg(provenance), "dch")?;

    let mut changelog = ChangeLog::read_path(staged_path).context("read prepared changelog")?;
    normalize_top_entry(&mut changelog, provenance)?;
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
        if change.try_version().transpose()? != Some(top_version.clone()) {
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

#[cfg(test)]
mod tests {
    use indoc::{formatdoc, indoc};

    use super::*;

    /// Parses changelog text and returns its top entry fields.
    fn parse_text(contents: &str) -> TopChangelog {
        parse_top_changelog(&contents.parse().unwrap()).unwrap()
    }

    #[test]
    /// Verifies top changelog parsing and Cargo-version validation with a repack suffix.
    fn validates_top_changelog_identity() {
        let top = parse_text(indoc! {r"
            rust-example (1.2.3+ds-0ubuntu1) noble; urgency=medium
        "});
        validate_top_changelog(&top, "1.2.3", "1.2.3").unwrap();
        assert_eq!(top.source, "rust-example");
        assert_eq!(top.upstream, "1.2.3+ds");
    }

    #[test]
    /// Creates, increments, or updates entries while preserving existing changes.
    fn prepares_changelog_entries() {
        for (distribution, upstream, version, entries) in [
            (None, "1.0.0", "1.0.0-0ubuntu1", 1),
            (Some("noble"), "1.0.0", "1.0.0-0ubuntu2", 2),
            (Some("noble"), "2.0.0", "2.0.0-0ubuntu1", 2),
            (Some("UNRELEASED"), "1.0.0", "1.0.0-0ubuntu1", 1),
            (Some("UNRELEASED"), "2.0.0", "2.0.0-0ubuntu1", 1),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let old_path = directory.path().join("old");
            let staged_path = directory.path().join("changelog");
            let old_top = distribution.map(|distribution| {
                fs::write(
                    &old_path,
                    formatdoc! {r"
                        rust-example (1.0.0-0ubuntu1) {distribution}; urgency=medium

                          * Maintainer change.

                         -- Example <example@example.com>  Mon, 01 Jan 2024 00:00:00 +0000
                    "},
                )
                .unwrap();
                read_top_changelog(&old_path).unwrap()
            });
            let provenance = formatdoc! {r"
                Package example {upstream} from crates.io.
                  Generated with debcargo 2.8.4 and ubucargo {}.",
                env!("CARGO_PKG_VERSION")
            };
            prepare_changelog(
                old_top.as_ref().map(|_| old_path),
                &staged_path,
                old_top.as_ref(),
                "rust-example",
                upstream,
                &provenance,
            )
            .unwrap();
            let changelog = ChangeLog::read_path(&staged_path).unwrap();
            let top = parse_top_changelog(&changelog).unwrap();
            assert_eq!(top.version, version);
            assert_eq!(top.distribution, "UNRELEASED");
            assert_eq!(changelog.iter().count(), entries);
            assert_eq!(
                changelog
                    .to_string()
                    .matches("Generated with debcargo")
                    .count(),
                1
            );
            if old_top.is_some() {
                assert!(changelog.to_string().contains("Maintainer change."));
            }
        }
    }

    #[test]
    /// Verifies provenance replacement.
    fn replaces_provenance_once() {
        let old = indoc! {r"
            rust-example (1.0.0-1) UNRELEASED; urgency=medium

              * local change
              * Package example 0.9.0 from crates.io.
                Generated with debcargo 2.8.4 and ubucargo 0.1.0.
              * Package example 1.0.0 from crates.io.
                Generated with debcargo 2.8.4 and ubucargo 0.1.0.

             -- A <a@example.com>  Mon, 01 Jan 2024 00:00:00 +0000

            rust-example (0.9.0-1) unstable; urgency=medium

              * Package example 0.9.0 from crates.io.
                Generated with debcargo 2.8.4 and ubucargo 0.1.0.

             -- B <b@example.com>  Sun, 31 Dec 2023 00:00:00 +0000
        "};
        let mut changelog: ChangeLog = old.parse().unwrap();
        normalize_top_entry(
            &mut changelog,
            indoc! {r"
                Package example 1.0.0 from crates.io.
                  Generated with debcargo 2.8.4 and ubucargo 0.1.0."},
        )
        .unwrap();
        let new = changelog.to_string();
        assert_eq!(new.matches("debcargo").count(), 2);
        assert!(new.contains("* Package example 0.9.0 from crates.io."));
        assert!(new.contains("  * local change"));
        assert!(new.contains(concat!(
            "  ",
            indoc! {r"
                * Package example 1.0.0 from crates.io.
                    Generated with debcargo 2.8.4 and ubucargo 0.1.0."}
        )));
    }
}
