//! Extracts Rust dependency requirements from generated Debian control files.

use std::{collections::BTreeMap, fs, path::Path, process::Command};

use anyhow::{Context, Result, bail};
use debian_control::{
    lossy::{Control, Relation, Relations},
    relations::{BuildProfile, VersionConstraint},
};
use debversion::Version;

use crate::package::MetadataDependency;

/// One Debian package alternative in a dependency expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageRequirement {
    /// Binary or virtual package name.
    pub name: String,
    /// Optional Debian version constraint.
    pub version: Option<(VersionConstraint, Version)>,
}

/// Debian requirements grouped by Cargo feature, with `None` for the base crate.
pub type FeatureRequirements = BTreeMap<Option<String>, Vec<Vec<PackageRequirement>>>;

/// Cargo and Debian requirements belonging to one direct Rust crate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependency {
    /// Normalized Cargo crate name.
    pub name: String,
    /// Cargo semantic-version requirement from the patch-applied manifest.
    pub cargo_requirement: String,
    /// Debian package requirements used for availability and feature checks.
    pub debian_requirements: FeatureRequirements,
}

/// Reads direct Rust dependencies from generated control and Cargo metadata.
pub fn read_dependencies(
    path: &Path,
    architecture: &str,
    cargo_dependencies: &[MetadataDependency],
) -> Result<Vec<Dependency>> {
    let contents = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_dependencies(&contents, architecture, cargo_dependencies)
        .with_context(|| format!("parse Rust dependencies from {}", path.display()))
}

/// Parses and groups direct Rust dependencies by crate and feature.
fn parse_dependencies(
    contents: &str,
    architecture: &str,
    cargo_dependencies: &[MetadataDependency],
) -> Result<Vec<Dependency>> {
    let source = contents.split("\n\n").next().unwrap_or(contents);
    let control: Control = source.parse().map_err(anyhow::Error::msg)?;
    let mut grouped = BTreeMap::new();
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
    for (name, debian_requirements) in grouped {
        let mut cargo_requirement = None;
        for dependency in cargo_dependencies {
            if normalize_crate_name(&dependency.name) != name {
                continue;
            }
            if cargo_requirement
                .as_ref()
                .is_some_and(|existing| existing != &dependency.req)
            {
                bail!("Cargo has multiple version requirements for dependency {name}");
            }
            cargo_requirement = Some(dependency.req.clone());
        }
        dependencies.push(Dependency {
            name: name.clone(),
            cargo_requirement: cargo_requirement
                .with_context(|| format!("Cargo metadata has no dependency {name}"))?,
            debian_requirements,
        });
    }
    Ok(dependencies)
}

/// Adds applicable Rust relations to their crate and feature groups.
fn collect_dependencies(
    relations: &Relations,
    architecture: &str,
    grouped: &mut BTreeMap<String, FeatureRequirements>,
) -> Result<()> {
    for entry in relations.iter() {
        let mut alternatives = Vec::new();
        let mut identity = None;
        for relation in &entry {
            if !relation_applies(relation, architecture)? {
                continue;
            }
            let Some((name, _, feature)) = parse_rust_package_name(&relation.name) else {
                continue;
            };
            if identity
                .as_ref()
                .is_some_and(|existing| existing != &(name.clone(), feature.clone()))
            {
                bail!("Rust dependency alternatives refer to different features: {entry:?}");
            }
            identity = Some((name, feature));
            alternatives.push(PackageRequirement {
                name: relation.name.clone(),
                version: relation.version.clone(),
            });
        }
        if let Some((name, feature)) = identity {
            grouped
                .entry(name)
                .or_default()
                .entry(feature)
                .or_default()
                .push(alternatives);
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

/// Normalizes Cargo crate spelling to Debian's dashed lowercase form.
fn normalize_crate_name(name: &str) -> String {
    name.replace('_', "-").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates one Cargo metadata dependency for parser tests.
    fn cargo_dependency(name: &str, requirement: &str) -> MetadataDependency {
        MetadataDependency {
            name: name.to_owned(),
            req: requirement.to_owned(),
        }
    }

    #[test]
    /// Groups Debian requirements by feature and attaches Cargo requirements.
    fn extracts_rust_dependencies() {
        let control = r#"Source: rust-example
Build-Depends: debhelper-compat (= 13),
 librust-serde-1+derive-dev (>= 1.0.100-~~),
 librust-serde-1+std-dev,
 librust-syn-2-dev | librust-syn-dev,
 librust-disabled-1-dev [arm64]

Package: librust-example-dev
Architecture: any
Description: example
"#;
        let dependencies = parse_dependencies(
            control,
            "amd64",
            &[
                cargo_dependency("serde", "^1.0.100"),
                cargo_dependency("syn", "^2"),
            ],
        )
        .unwrap();

        assert_eq!(dependencies.len(), 2);
        assert_eq!(dependencies[0].cargo_requirement, "^1.0.100");
        assert_eq!(dependencies[0].debian_requirements.len(), 2);
        assert_eq!(dependencies[1].cargo_requirement, "^2");
        assert_eq!(dependencies[1].debian_requirements[&None][0].len(), 2);
    }

    #[test]
    /// Rejects alternatives that cannot belong to one Cargo feature.
    fn rejects_mixed_feature_alternatives() {
        let control = r#"Source: rust-example
Build-Depends: librust-serde+alloc-dev | librust-serde+std-dev

Package: librust-example-dev
Architecture: any
Description: example
"#;
        assert!(parse_dependencies(control, "amd64", &[cargo_dependency("serde", "^1")]).is_err());
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
