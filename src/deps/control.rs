//! Extracts Rust dependency requirements from generated Debian control files.

use std::{collections::BTreeMap, fs, path::Path, process::Command};

use anyhow::{Context, Result, bail};
use debian_control::{
    lossless::control::Control,
    lossy::{Relation, Relations},
    relations::{BuildProfile, VersionConstraint},
};
use debversion::Version;

/// One Debian package relation in a dependency expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageRequirement {
    /// Binary or virtual package name.
    pub name: String,
    /// Optional Debian version constraint.
    pub version: Option<(VersionConstraint, Version)>,
}

/// Debian requirements grouped by Cargo feature, with `None` for the base crate.
/// Every relation in a vector is required (comma-separated AND).
pub type FeatureRequirements = BTreeMap<Option<String>, Vec<PackageRequirement>>;

/// Debian requirements belonging to one Rust crate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependency {
    /// Normalized Cargo crate name.
    pub name: String,
    /// Debian package requirements used for availability and feature checks.
    pub debian_requirements: FeatureRequirements,
}

/// Reads and groups Rust dependencies from generated Debian control metadata.
pub fn read_dependencies(control_path: &Path, architecture: &str) -> Result<Vec<Dependency>> {
    let contents = fs::read_to_string(control_path)
        .with_context(|| format!("read {}", control_path.display()))?;
    // We use the lossless (i.e. less strongly typed) parser at the top level for Control,
    // since the lossy parser would fail to parse substitutions (e.g. "${misc:Depends}")
    // in parts that we don't need to look at.
    let control: Control = contents
        .parse()
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("parse {}", control_path.display()))?;
    let source = control
        .source()
        .context("control file has no source paragraph")?;
    let mut grouped = BTreeMap::new();
    for field in ["Build-Depends", "Build-Depends-Arch", "Build-Depends-Indep"] {
        let Some(value) = source.get(field) else {
            continue;
        };
        let relations: Relations = value
            .parse()
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("parse {field} in {}", control_path.display()))?;
        collect_dependencies(&relations, architecture, &mut grouped)
            .with_context(|| format!("parse Rust dependencies from {}", control_path.display()))?;
    }

    let mut dependencies = Vec::new();
    for (name, debian_requirements) in grouped {
        dependencies.push(Dependency {
            name,
            debian_requirements,
        });
    }
    Ok(dependencies)
}

/// Adds applicable Rust relations to their crate and feature groups,
/// rejecting `|` alternatives involving Rust packages, which debcargo never generates.
fn collect_dependencies(
    relations: &Relations,
    architecture: &str,
    grouped: &mut BTreeMap<String, FeatureRequirements>,
) -> Result<()> {
    for entry in relations.iter() {
        let mut applicable = Vec::new();
        for relation in &entry {
            if relation_applies(relation, architecture)? {
                applicable.push(relation);
            }
        }
        let [relation] = applicable.as_slice() else {
            for relation in &applicable {
                if parse_rust_package_name(&relation.name).is_some() {
                    bail!("Rust dependency alternatives are not supported: {entry:?}");
                }
            }
            continue;
        };
        let Some((name, feature)) = parse_rust_package_name(&relation.name) else {
            continue;
        };
        grouped
            .entry(name.to_owned())
            .or_default()
            .entry(feature.map(str::to_owned))
            .or_default()
            .push(PackageRequirement {
                name: relation.name.clone(),
                version: relation.version.clone(),
            });
    }
    Ok(())
}

/// Reports whether a relation applies for the selected architecture and default profiles.
fn relation_applies(relation: &Relation, architecture: &str) -> Result<bool> {
    if let Some(architectures) = &relation.architectures {
        let mut has_positive = false;
        let mut positive_match = false;
        for candidate in architectures {
            if let Some(excluded) = candidate.strip_prefix('!') {
                if matches_architecture(architecture, excluded)? {
                    return Ok(false);
                }
            } else {
                has_positive = true;
                if matches_architecture(architecture, candidate)? {
                    positive_match = true;
                }
            }
        }
        if has_positive && !positive_match {
            return Ok(false);
        }
    }
    let profile_valid = relation.profiles.is_empty()
        || relation.profiles.iter().any(|group| {
            group
                .iter()
                .all(|profile| matches!(profile, BuildProfile::Disabled(_)))
        });
    Ok(profile_valid)
}

/// Matches one Debian architecture against an architecture restriction.
fn matches_architecture(architecture: &str, restriction: &str) -> Result<bool> {
    let status = Command::new("dpkg-architecture")
        .arg(format!("-a{architecture}"))
        .arg(format!("-i{restriction}"))
        .status()
        .context("run dpkg-architecture")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("dpkg-architecture could not match {architecture:?} against {restriction:?}"),
    }
}

/// Extracts a crate and feature from a Debian Rust package, stripping its semver suffix.
pub fn parse_rust_package_name(package: &str) -> Option<(&str, Option<&str>)> {
    let body = package.strip_prefix("librust-")?.strip_suffix("-dev")?;
    let (base, feature) = match body.split_once('+') {
        Some((base, feature)) => (base, Some(feature)),
        None => (body, None),
    };
    let mut name = base;
    if let Some((prefix, suffix)) = base.rsplit_once('-')
        && suffix
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        name = prefix;
    }
    Some((name, feature))
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use super::*;

    #[test]
    /// Groups Debian requirements by crate and feature without requiring Cargo metadata.
    fn extracts_rust_dependencies() {
        let control = indoc! {r#"
            Source: rust-example
            Build-Depends: debhelper-compat (= 13),
             librust-serde-1+derive-dev (>= 1.0.100-~~),
             librust-serde-1+std-dev,
             librust-syn-2-dev,
             librust-disabled-1-dev [arm64]

            Package: librust-example-dev
            Architecture: any
            Provides: librust-example-1-dev (= ${binary:Version}), ${cargo:Provides}
            Depends: ${misc:Depends}, ${cargo:Depends}
            Description: example
        "#};
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), control).unwrap();
        let dependencies = read_dependencies(file.path(), "amd64").unwrap();

        assert_eq!(dependencies.len(), 2);
        assert_eq!(dependencies[0].name, "serde");
        assert_eq!(dependencies[0].debian_requirements.len(), 2);
        assert_eq!(dependencies[1].name, "syn");
        assert_eq!(dependencies[1].debian_requirements[&None].len(), 1);
    }

    #[test]
    /// Rejects malformed dependencies in each source Build-Depends field.
    fn rejects_invalid_source_dependencies() {
        let file = tempfile::NamedTempFile::new().unwrap();
        for field in ["Build-Depends", "Build-Depends-Arch", "Build-Depends-Indep"] {
            fs::write(
                file.path(),
                format!("Source: rust-example\n{field}: librust-serde-dev (>= )\n"),
            )
            .unwrap();
            let error = read_dependencies(file.path(), "amd64").unwrap_err();
            assert!(error.to_string().contains(&format!("parse {field}")));
        }
    }

    #[test]
    /// Rejects applicable alternatives involving Rust packages.
    fn rejects_rust_alternatives() {
        let file = tempfile::NamedTempFile::new().unwrap();
        for relations in [
            "librust-serde-1-dev | librust-serde-2-dev",
            "librust-serde+alloc-dev | librust-serde+std-dev",
            "librust-serde-dev | librust-syn-dev",
            "librust-serde-dev | other-package",
            "other-package | librust-serde-dev",
        ] {
            fs::write(
                file.path(),
                format!("Source: rust-example\nBuild-Depends: {relations}\n"),
            )
            .unwrap();
            let error = read_dependencies(file.path(), "amd64").unwrap_err();
            assert!(
                format!("{error:#}").contains("alternatives are not supported"),
                "{relations}: {error:#}"
            );
        }
    }

    #[test]
    /// Ignores non-Rust groups and drops inapplicable alternatives before rejecting the rest.
    fn accepts_applicable_alternatives() {
        let file = tempfile::NamedTempFile::new().unwrap();
        for (relations, expected_rust) in [
            ("other-package | another-package", false),
            ("librust-serde-dev | other-package [arm64]", true),
            ("other-package [arm64] | librust-serde-dev", true),
            ("librust-serde-dev | other-package <stage1>", true),
            ("other-package <stage1> | librust-serde-dev", true),
            ("librust-serde-dev [arm64] | other-package", false),
            ("other-package | librust-serde-dev <stage1>", false),
            ("librust-serde-dev [arm64] | other-package <stage1>", false),
        ] {
            fs::write(
                file.path(),
                format!("Source: rust-example\nBuild-Depends: {relations}\n"),
            )
            .unwrap();
            let dependencies = read_dependencies(file.path(), "amd64").unwrap();
            if expected_rust {
                assert_eq!(dependencies.len(), 1, "{relations}");
                assert_eq!(dependencies[0].name, "serde", "{relations}");
                assert_eq!(
                    dependencies[0].debian_requirements[&None].len(),
                    1,
                    "{relations}"
                );
            } else {
                assert!(dependencies.is_empty(), "{relations}");
            }
        }
    }

    #[test]
    /// Parses crate names containing digits without confusing them with semver suffixes.
    fn parses_rust_package_names() {
        assert_eq!(
            parse_rust_package_name("librust-sha2-0.10+default-dev"),
            Some(("sha2", Some("default")))
        );
        assert_eq!(
            parse_rust_package_name("librust-serde-dev"),
            Some(("serde", None))
        );
        for (package, name) in [
            ("librust-sha2-dev", "sha2"),
            ("librust-some-crate-1.2-dev", "some-crate"),
            ("librust-some-1-crate-dev", "some-1-crate"),
            ("librust-some-1..2-dev", "some-1..2"),
            ("librust-some--dev", "some-"),
        ] {
            assert_eq!(parse_rust_package_name(package), Some((name, None)));
        }
        assert_eq!(parse_rust_package_name("serde-dev"), None);
        assert_eq!(parse_rust_package_name("librust-serde"), None);
    }
}
