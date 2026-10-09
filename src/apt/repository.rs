//! Repository-specific configuration for the shared APT view.

use std::path::Path;

use anyhow::Result;
use indoc::formatdoc;

use super::{get_ppa_key, parse_ppa, validate_name};
use crate::input::{Distribution, Suite, split_archive_suite};

/// Repository selection expanded into authenticated APT source entries.
pub enum Repository<'a> {
    /// Ubuntu series or pocket, optionally including proposed.
    Ubuntu { suite: &'a str, proposed: bool },
    /// Exact Debian suite in the main archive.
    Debian { suite: &'a str },
    /// Public Launchpad PPA in an Ubuntu series.
    Ppa { ppa: &'a str, series: &'a str },
}

impl<'a> Repository<'a> {
    /// Selects the repository configuration for a distribution-qualified archive suite.
    pub fn select_archive(suite: &'a Suite, proposed: bool) -> Self {
        match suite.distribution {
            Distribution::Ubuntu => Self::Ubuntu {
                suite: &suite.name,
                proposed,
            },
            Distribution::Debian => Self::Debian { suite: &suite.name },
        }
    }

    /// Appends source entries, retrieving PPA keys into the locked view when needed.
    pub fn append_sources(
        &self,
        sources: &mut String,
        architecture: &str,
        keys: &Path,
    ) -> Result<()> {
        match self {
            Self::Ubuntu { suite, proposed } => {
                validate_name("suite", suite)?;
                let ports = !matches!(architecture, "amd64" | "i386");
                let archive = if ports {
                    "https://ports.ubuntu.com/ubuntu-ports"
                } else {
                    "https://archive.ubuntu.com/ubuntu"
                };
                let security = if ports {
                    archive
                } else {
                    "https://security.ubuntu.com/ubuntu"
                };
                let (_, pocket) = split_archive_suite(suite);
                let suites = if pocket.is_some() {
                    suite.to_string()
                } else if *proposed {
                    format!("{suite} {suite}-updates {suite}-proposed")
                } else {
                    format!("{suite} {suite}-updates")
                };
                let uri = if pocket == Some("security") {
                    security
                } else {
                    archive
                };
                let keyring = Path::new("/usr/share/keyrings/ubuntu-archive-keyring.gpg");
                append_source(
                    sources,
                    uri,
                    &suites,
                    "main universe",
                    architecture,
                    keyring,
                );
                if pocket.is_none() {
                    append_source(
                        sources,
                        security,
                        &format!("{suite}-security"),
                        "main universe",
                        architecture,
                        keyring,
                    );
                }
            }
            Self::Debian { suite } => {
                validate_name("suite", suite)?;
                append_source(
                    sources,
                    "https://deb.debian.org/debian",
                    suite,
                    "main",
                    architecture,
                    Path::new("/usr/share/keyrings/debian-archive-keyring.gpg"),
                );
            }
            Self::Ppa { ppa, series } => {
                validate_name("series", series)?;
                let (owner, name) = parse_ppa(ppa)?;
                let key = get_ppa_key(owner, name, keys)?;
                append_source(
                    sources,
                    &format!("https://ppa.launchpadcontent.net/{owner}/{name}/ubuntu"),
                    series,
                    "main",
                    architecture,
                    &key,
                );
            }
        }
        Ok(())
    }
}

/// Appends one deb822 entry with the shared index targets and authentication settings.
fn append_source(
    sources: &mut String,
    uri: &str,
    suites: &str,
    components: &str,
    architecture: &str,
    keyring: &Path,
) {
    sources.push_str(&formatdoc! {
        "
        Types: deb deb-src
        URIs: {uri}
        Suites: {suites}
        Components: {components}
        Architectures: {architecture}
        Targets: Packages Sources
        Signed-By: {}

        ",
        keyring.display()
    });
}

/// Formats repository provenance using distribution and PPA selectors.
pub fn format_location(uri: &str, release: &str, component: &str) -> String {
    let uri = uri.trim_end_matches('/');
    if let Some(path) = uri
        .strip_prefix("https://ppa.launchpadcontent.net/")
        .or_else(|| uri.strip_prefix("http://ppa.launchpadcontent.net/"))
        && let Some(path) = path.strip_suffix("/ubuntu")
    {
        return format!("ppa:{path} ({release})");
    }
    if matches!(
        uri,
        "https://deb.debian.org/debian" | "http://deb.debian.org/debian"
    ) {
        return format!("debian:{release}/{component}");
    }
    format!("{release}/{component}")
}
