//! Resolves canonical crate names and stable releases from crates.io metadata.

use anyhow::{Context, Result, bail};
use reqwest::{StatusCode, blocking::Client};
use semver::Version;
use serde::Deserialize;

use crate::{cargo::validate_crate_name, resolve::normalize_crate_name};

/// Published crate identity and non-yanked stable releases.
pub struct Crate {
    /// Canonical published package name.
    pub name: String,
    /// Non-yanked stable releases in descending semantic precedence.
    pub versions: Vec<Version>,
}

/// Identity and version records returned by the crate metadata endpoint.
#[derive(Deserialize)]
struct CrateMetadata {
    /// Published crate identity.
    #[serde(rename = "crate")]
    identity: CrateIdentity,
    /// Published releases in API order.
    versions: Vec<ReleaseMetadata>,
}

/// Canonical identity returned by crates.io.
#[derive(Deserialize)]
struct CrateIdentity {
    /// Published package name.
    name: String,
}

/// Fields needed to select a stable crates.io release.
#[derive(Deserialize)]
struct ReleaseMetadata {
    /// Semantic release version.
    num: String,
    /// Whether new resolution should exclude this release.
    yanked: bool,
}

/// Queries a crate's canonical name and stable releases, returning `None` for HTTP 404.
pub fn read_crate(crate_name: &str, api: &str) -> Result<Option<Crate>> {
    validate_crate_name(crate_name)?;
    let response = Client::builder()
        .user_agent(concat!("ubucargo/", env!("CARGO_PKG_VERSION")))
        .build()?
        .get(format!("{}/crates/{crate_name}", api.trim_end_matches('/')))
        .send()
        .with_context(|| format!("query crates.io for {crate_name}"))?;
    match response.status() {
        StatusCode::NOT_FOUND => return Ok(None),
        StatusCode::OK => {}
        status => bail!("crates.io lookup for {crate_name} failed: HTTP {status}"),
    }
    let metadata: CrateMetadata = response
        .json()
        .with_context(|| format!("parse crates.io metadata for {crate_name}"))?;
    if normalize_crate_name(&metadata.identity.name) != normalize_crate_name(crate_name) {
        bail!(
            "crates.io returned crate {} instead of {crate_name}",
            metadata.identity.name
        );
    }
    let mut versions = Vec::new();
    for release in metadata.versions {
        let version = Version::parse(&release.num)
            .with_context(|| format!("parse crates.io version for {crate_name}"))?;
        if release.yanked || !version.pre.is_empty() {
            continue;
        }
        versions.push(version);
    }
    versions.sort_by(|left, right| right.cmp_precedence(left));
    Ok(Some(Crate {
        name: metadata.identity.name,
        versions,
    }))
}
