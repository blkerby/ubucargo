//! Extracts Rust dependency requirements from Debian control and test files.

use std::{fs, io, path::Path, process::Command};

use anyhow::{Context, Result, bail};
use deb822_fast::Deb822;
use debian_control::{
    lossy::{Relation, Relations},
    relations::{BuildProfile, VersionConstraint},
};
use debversion::Version;

/// One Debian package relation in a dependency expression.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PackageRequirement {
    /// Binary or virtual package name.
    pub name: String,
    /// Optional Debian version constraint.
    pub version: Option<(VersionConstraint, Version)>,
}

/// Origin of dependency requirements in Debian packaging files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DependencyOrigin {
    /// A binary package's Depends field.
    Package,
    /// The source package's Build-Depends fields.
    Source,
    /// All autopkgtests' Depends fields.
    Tests,
}

/// Applicable Rust relations declared by a binary package, source package, or tests.
#[derive(Debug, Eq, PartialEq)]
pub struct DependencySection {
    /// Paragraph or file declaring these requirements.
    pub origin: DependencyOrigin,
    /// Binary package name, or source package name for the source and tests.
    pub name: String,
    /// Parsed relations in declaration order, including duplicates.
    pub requirements: Vec<PackageRequirement>,
}

/// Reads binary Depends, source Build-Depends fields, and autopkgtest Depends.
/// Retains empty sections and repeated relations for the caller to interpret.
pub fn read_dependency_sections(
    source_root: &Path,
    architecture: &str,
) -> Result<Vec<DependencySection>> {
    let control_path = source_root.join("debian/control");
    let contents = fs::read_to_string(&control_path)
        .with_context(|| format!("read {}", control_path.display()))?;
    let control: Deb822 = contents
        .parse()
        .with_context(|| format!("parse {}", control_path.display()))?;
    let mut sections = Vec::new();
    let mut source = None;
    for paragraph in control.iter() {
        let mut requirements = Vec::new();
        if let Some(name) = paragraph.get("Source") {
            for field in ["Build-Depends", "Build-Depends-Arch", "Build-Depends-Indep"] {
                if let Some(value) = paragraph.get(field) {
                    collect_requirements(value, architecture, &mut requirements)
                        .with_context(|| format!("parse {field} in {}", control_path.display()))?;
                }
            }
            source = Some((name.to_owned(), requirements));
        } else if let Some(name) = paragraph.get("Package") {
            if let Some(value) = paragraph.get("Depends") {
                collect_requirements(value, architecture, &mut requirements).with_context(
                    || format!("parse Depends of {name} in {}", control_path.display()),
                )?;
            }
            sections.push(DependencySection {
                origin: DependencyOrigin::Package,
                name: name.to_owned(),
                requirements,
            });
        }
    }
    let (source_name, build_requirements) =
        source.context("control file has no source paragraph")?;
    sections.push(DependencySection {
        origin: DependencyOrigin::Source,
        name: source_name.clone(),
        requirements: build_requirements,
    });

    let tests_path = source_root.join("debian/tests/control");
    let mut test_requirements = Vec::new();
    match fs::read_to_string(&tests_path) {
        Ok(contents) => {
            let tests: Deb822 = contents
                .parse()
                .with_context(|| format!("parse {}", tests_path.display()))?;
            // Test Architecture restrictions are ignored, so a test that never
            // runs on the selected architecture still contributes its dependencies.
            for paragraph in tests.iter() {
                if let Some(value) = paragraph.get("Depends") {
                    collect_requirements(value, architecture, &mut test_requirements)
                        .with_context(|| format!("parse Depends in {}", tests_path.display()))?;
                }
            }
        }
        // Debcargo writes no tests for crates without a library.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", tests_path.display()));
        }
    }
    sections.push(DependencySection {
        origin: DependencyOrigin::Tests,
        name: source_name,
        requirements: test_requirements,
    });
    Ok(sections)
}

/// Adds the applicable Rust relations from one relation field,
/// rejecting `|` alternatives involving Rust packages, which debcargo never generates.
fn collect_requirements(
    value: &str,
    architecture: &str,
    requirements: &mut Vec<PackageRequirement>,
) -> Result<()> {
    // Debcargo writes substitution variables only in entries such as `${misc:Depends}` and in
    // relations to its own packages, such as `librust-foo-dev (= ${binary:Version})`, and
    // autopkgtest uses `@` entries for the tested packages; none of these need checking.
    let mut entries = Vec::new();
    for entry in value.split(',') {
        let entry = entry.trim();
        if !entry.contains('$') && !entry.starts_with('@') {
            entries.push(entry);
        }
    }
    let relations: Relations = entries.join(", ").parse().map_err(anyhow::Error::msg)?;
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
        if parse_rust_package_name(&relation.name).is_some() {
            requirements.push(PackageRequirement {
                name: relation.name.clone(),
                version: relation.version.clone(),
            });
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

/// Splits a Debian Rust package name into its crate, optional semver line, and optional feature.
pub fn parse_rust_package_name(package: &str) -> Option<(&str, Option<&str>, Option<&str>)> {
    let body = package.strip_prefix("librust-")?.strip_suffix("-dev")?;
    let (base, feature) = match body.split_once('+') {
        Some((base, feature)) => (base, Some(feature)),
        None => (body, None),
    };
    let mut name = base;
    let mut line = None;
    if let Some((prefix, suffix)) = base.rsplit_once('-')
        && suffix
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        name = prefix;
        line = Some(suffix);
    }
    Some((name, line, feature))
}
