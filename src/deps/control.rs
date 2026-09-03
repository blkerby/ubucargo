//! Extracts Rust dependency requirements from generated Debian control files.

use std::process::Command;
use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, bail};
use debian_control::{
    lossy::{Control, Relation, Relations},
    relations::{BuildProfile, VersionConstraint},
};
use debversion::Version;
use semver::{Version as SemverVersion, VersionReq};

/// One Debian package alternative in a dependency expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageRequirement {
    /// Binary or virtual package name.
    pub name: String,
    /// Optional Debian version constraint.
    pub version: Option<(VersionConstraint, Version)>,
}

/// All generated Debian requirements belonging to one direct Rust crate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependency {
    /// Normalized Cargo crate name.
    pub name: String,
    /// Human-readable Cargo-style semver and feature requirement.
    pub requirement: String,
    /// Comma-separated requirements containing pipe-separated alternatives.
    pub entries: Vec<Vec<PackageRequirement>>,
}

/// Requirements accumulated while grouping generated package relations by crate.
#[derive(Default)]
struct DependencyParts {
    /// Comma-separated package requirements and their alternatives.
    entries: Vec<Vec<PackageRequirement>>,
    /// Semver lines and strongest lower bounds encoded in Debian relations.
    versions: BTreeMap<String, Option<SemverVersion>>,
    /// Cargo features encoded in Debian virtual package names.
    features: Vec<String>,
}

/// Reads direct Rust build dependencies from a generated control file.
pub fn read_dependencies(path: &Path, architecture: &str) -> Result<Vec<Dependency>> {
    let contents = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_dependencies(&contents, architecture)
        .with_context(|| format!("parse Rust dependencies from {}", path.display()))
}

/// Parses direct Rust build dependencies from generated control text.
fn parse_dependencies(contents: &str, architecture: &str) -> Result<Vec<Dependency>> {
    let source = contents.split("\n\n").next().unwrap_or(contents);
    let control: Control = source.parse().map_err(anyhow::Error::msg)?;
    let mut grouped: BTreeMap<String, DependencyParts> = BTreeMap::new();
    for relations in [
        control.source.build_depends,
        control.source.build_depends_arch,
        control.source.build_depends_indep,
    ]
    .into_iter()
    .flatten()
    {
        collect_dependencies(&relations, architecture, &mut grouped)?;
    }

    let mut dependencies = Vec::new();
    for (name, mut parts) in grouped {
        parts.features.sort();
        parts.features.dedup();
        let mut requirement = if parts.versions.is_empty() {
            "*".to_owned()
        } else {
            parts
                .versions
                .into_iter()
                .map(|(line, minimum)| {
                    format!("^{}", minimum.map_or(line, |value| value.to_string()))
                })
                .collect::<Vec<_>>()
                .join("|")
        };
        for feature in parts.features {
            requirement.push_str(" +");
            requirement.push_str(&feature);
        }
        dependencies.push(Dependency {
            name,
            requirement,
            entries: parts.entries,
        });
    }
    Ok(dependencies)
}

/// Adds applicable Rust relations to their logical crate dependency.
fn collect_dependencies(
    relations: &Relations,
    architecture: &str,
    grouped: &mut BTreeMap<String, DependencyParts>,
) -> Result<()> {
    for entry in relations.iter() {
        let mut alternatives = Vec::new();
        let mut crate_name = None;
        let mut versions = BTreeMap::new();
        let mut features = Vec::new();
        for relation in &entry {
            if !relation_applies(relation, architecture)? {
                continue;
            }
            let Some((name, version, feature)) = parse_rust_package_name(&relation.name) else {
                continue;
            };
            if crate_name
                .as_ref()
                .is_some_and(|existing| existing != &name)
            {
                bail!("Rust dependency alternatives refer to different crates: {entry:?}");
            }
            crate_name = Some(name);
            if let Some(version) = version {
                let minimum = match &relation.version {
                    Some((VersionConstraint::GreaterThanEqual, minimum)) => {
                        let minimum = minimum
                            .upstream_version
                            .strip_suffix("-~~")
                            .unwrap_or(&minimum.upstream_version);
                        SemverVersion::parse(minimum).ok().filter(|minimum| {
                            VersionReq::parse(&format!("^{version}"))
                                .is_ok_and(|line| line.matches(minimum))
                        })
                    }
                    _ => None,
                };
                match versions.entry(version) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(minimum);
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        if minimum < *entry.get() {
                            entry.insert(minimum);
                        }
                    }
                }
            }
            if let Some(feature) = feature {
                features.push(feature);
            }
            alternatives.push(PackageRequirement {
                name: relation.name.clone(),
                version: relation.version.clone(),
            });
        }
        if let Some(crate_name) = crate_name {
            let grouped = grouped.entry(crate_name).or_default();
            grouped.entries.push(alternatives);
            for (version, minimum) in versions {
                let current = grouped.versions.entry(version).or_default();
                if minimum > *current {
                    *current = minimum;
                }
            }
            grouped.features.extend(features);
        }
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
    if relation.profiles.is_empty() {
        return Ok(true);
    }
    for group in &relation.profiles {
        let mut matches = true;
        for profile in group {
            match profile {
                BuildProfile::Enabled(_) => matches = false,
                BuildProfile::Disabled(_) => {}
            }
        }
        if matches {
            return Ok(true);
        }
    }
    Ok(false)
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

/// Splits a Debian Rust development package into crate, semver line, and feature.
pub(crate) fn parse_rust_package_name(
    package: &str,
) -> Option<(String, Option<String>, Option<String>)> {
    let body = package.strip_prefix("librust-")?.strip_suffix("-dev")?;
    let (base, feature) = match body.split_once('+') {
        Some((base, feature)) => (base, Some(feature.to_owned())),
        None => (body, None),
    };
    let mut name = base;
    let mut version = None;
    for (index, character) in base.char_indices().rev() {
        if character != '-' {
            continue;
        }
        let suffix = &base[index + 1..];
        if !suffix.is_empty()
            && suffix
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            name = &base[..index];
            version = Some(suffix.to_owned());
            break;
        }
    }
    Some((name.to_owned(), version, feature))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Extracts and groups semver and feature package requirements.
    fn extracts_rust_dependencies() {
        let control = r#"Source: rust-example
Build-Depends: debhelper-compat (= 13),
 librust-serde-1+derive-dev (>= 1.0.100-~~),
 librust-serde-1+std-dev,
 librust-syn-2-dev | librust-syn-dev,
 librust-disabled-1-dev [arm64]

Package: librust-example-dev
Architecture: any
Provides: ${cargo:Provides}
Description: example
"#;
        let dependencies = parse_dependencies(control, "amd64").unwrap();
        assert_eq!(dependencies.len(), 2);
        assert_eq!(dependencies[0].name, "serde");
        assert_eq!(dependencies[0].requirement, "^1.0.100 +derive +std");
        assert_eq!(dependencies[0].entries.len(), 2);
        assert_eq!(dependencies[1].name, "syn");
        assert_eq!(dependencies[1].requirement, "^2");
        assert_eq!(dependencies[1].entries[0].len(), 2);
    }

    #[test]
    /// Includes a Debian lower bound in the effective Cargo-style requirement.
    fn includes_lower_bound_in_requirement() {
        let control = r#"Source: rust-example
Build-Depends: librust-actix-http-3+default-dev (>= 3.13.0)

Package: librust-example-dev
Architecture: any
Description: example
"#;
        let dependencies = parse_dependencies(control, "amd64").unwrap();

        assert_eq!(dependencies[0].requirement, "^3.13.0 +default");
    }

    #[test]
    /// Parses crate names containing digits without confusing them with semver suffixes.
    fn parses_rust_package_names() {
        assert_eq!(
            parse_rust_package_name("librust-sha2-0.10+default-dev"),
            Some((
                "sha2".to_owned(),
                Some("0.10".to_owned()),
                Some("default".to_owned())
            ))
        );
        assert_eq!(
            parse_rust_package_name("librust-serde-dev"),
            Some(("serde".to_owned(), None, None))
        );
    }
}
