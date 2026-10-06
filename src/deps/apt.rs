//! Builds an isolated APT view and reads Rust package candidates from it.

use std::{
    collections::BTreeMap,
    env, fs,
    io::{BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};

use crate::util::{run_command, write_file};
use deb822_fast::{Deb822, FromDeb822Paragraph};
use debian_control::{lossy::apt::Package, relations::VersionConstraint};
use debversion::Version;
use indoc::formatdoc;
use serde::Deserialize;

const UBUNTU_KEYRING: &str = "/usr/share/keyrings/ubuntu-archive-keyring.gpg";

/// One source package version from one configured repository location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageCandidate {
    /// Debian source package name.
    pub source: String,
    /// Debian source package version.
    pub version: Version,
    /// Concrete and virtual package names and versions supplied by its Rust binary packages.
    pub provides: BTreeMap<String, Option<Version>>,
    /// Compact repository location displayed to the user.
    pub location: String,
}

/// Strong digest algorithm advertised by an authenticated source index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChecksumAlgorithm {
    /// SHA512, preferred when available.
    Sha512,
    /// SHA256 for indexes that do not advertise SHA512.
    Sha256,
}

/// Authenticated source publication from a configured repository.
#[derive(Clone, Debug)]
pub struct SourceCandidate {
    /// Exact source-package name.
    pub source: String,
    /// Debian source version.
    pub version: Version,
    /// Repository provenance.
    pub location: String,
    /// Name of the source descriptor.
    pub dsc: String,
    /// Strong digest from the signed source index.
    pub checksum: String,
    /// Algorithm used to verify the descriptor digest.
    pub checksum_algorithm: ChecksumAlgorithm,
    /// Expected descriptor size.
    pub size: u64,
}

/// Binary and source records from the same locked APT query.
pub struct RepositoryRecords {
    /// Rust binary candidates used for dependency classification.
    pub packages: Vec<PackageCandidate>,
    /// Source publications used for input selection.
    pub sources: Vec<SourceCandidate>,
}

/// Launchpad archive metadata used to configure one public PPA.
#[derive(Deserialize)]
struct LaunchpadArchive {
    /// Whether Launchpad requires authentication for this archive.
    private: bool,
    /// OpenPGP fingerprint advertised for the archive signing key.
    signing_key_fingerprint: Option<String>,
}

/// Persistent APT configuration locked for one metadata query.
struct AptView {
    /// Shared configuration, indexes, signing keys, and binary cache.
    root: PathBuf,
    /// Keeps other invocations from changing the view until parsing finishes.
    _lock: fs::File,
    /// Selected binary architecture.
    architecture: String,
}

impl AptView {
    /// Adds the isolated APT configuration to a command.
    fn configure(&self, command: &mut Command) {
        let root = &self.root;
        for option in [
            format!(
                "Dir::Etc::sourcelist={}",
                root.join("sources.sources").display()
            ),
            format!(
                "Dir::Etc::sourceparts={}",
                root.join("sourceparts").display()
            ),
            format!("Dir::State::lists={}/", root.join("lists").display()),
            format!("Dir::State::status={}", root.join("status").display()),
            format!(
                "Dir::Etc::preferences={}",
                root.join("preferences").display()
            ),
            format!(
                "Dir::Etc::preferencesparts={}",
                root.join("preferences.d").display()
            ),
            format!(
                "Dir::Cache::pkgcache={}",
                root.join("pkgcache.bin").display()
            ),
            "Dir::Cache::srcpkgcache=".to_owned(),
            "APT::Get::List-Cleanup=0".to_owned(),
            "Acquire::Languages=none".to_owned(),
            "Acquire::GzipIndexes=false".to_owned(),
            format!("APT::Architecture={}", self.architecture),
        ] {
            command.arg("-o").arg(option);
        }
    }
}

/// Refreshes the selected repositories and returns their Rust package records.
pub fn load_records(
    series: &str,
    architecture: &str,
    proposed: bool,
    ppas: &[String],
) -> Result<RepositoryRecords> {
    validate_name("series", series)?;
    validate_name("architecture", architecture)?;
    let view = prepare_view(&cache_root()?, series, architecture, proposed, ppas)?;

    let mut update = Command::new("apt-get");
    view.configure(&mut update);
    // Retain any existing package cache, to be validated and reused below by indextargets if possible.
    update.args([
        "-o",
        "pkgCacheFile::Generate=false",
        "-o",
        "APT::Update::Error-Mode=any",
    ]);
    run_command(update.arg("update"), "apt-get update")?;

    let mut indexes = Command::new("apt-get");
    view.configure(&mut indexes);
    let output = run_command(
        indexes.args([
            "indextargets",
            "--format",
            "$(FILENAME)|$(SITE)|$(RELEASE)|$(COMPONENT)|$(ARCHITECTURE)|$(IDENTIFIER)",
        ]),
        "apt-get indextargets",
    )?;

    let mut candidates = Vec::new();
    let mut sources = Vec::new();
    let mut candidate_indexes = BTreeMap::new();
    for line in String::from_utf8(output.stdout)?.lines() {
        let fields: Vec<_> = line.split('|').collect();
        let [
            filename,
            site,
            release,
            component,
            index_architecture,
            identifier,
        ] = fields.as_slice()
        else {
            bail!(
                "unexpected apt-get indextargets row: expected 6 fields, got {}: {line:?}",
                fields.len()
            );
        };
        let location = format_location(site, release, component);
        if *identifier == "Sources" {
            read_sources(Path::new(filename), &location, &mut sources)?;
            continue;
        }
        if *identifier != "Packages" || *index_architecture != architecture {
            continue;
        }
        read_index(
            Path::new(filename),
            &location,
            &mut candidates,
            &mut candidate_indexes,
        )?;
    }
    Ok(RepositoryRecords {
        packages: candidates,
        sources,
    })
}

/// Locks the shared APT view, replacing its sources only when their contents change.
fn prepare_view(
    cache: &Path,
    series: &str,
    architecture: &str,
    proposed: bool,
    ppas: &[String],
) -> Result<AptView> {
    fs::create_dir_all(cache).context("create APT cache directory")?;
    let cache = cache.canonicalize()?;
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("view.lock"))?;
    lock.lock().context("lock shared APT view")?;
    for directory in ["sourceparts", "preferences.d", "lists/partial", "keys"] {
        fs::create_dir_all(cache.join(directory))?;
    }
    for file in ["status", "preferences"] {
        fs::File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(cache.join(file))?;
    }

    let mut sources = String::new();
    for ppa in ppas {
        let (owner, name) = parse_ppa(ppa)?;
        let key = get_ppa_key(owner, name, &cache.join("keys"))?;
        sources.push_str(&formatdoc! {
            "
            Types: deb deb-src
            URIs: https://ppa.launchpadcontent.net/{owner}/{name}/ubuntu
            Suites: {series}
            Components: main
            Architectures: {architecture}
            Targets: Packages Sources
            Signed-By: {}

            ",
            key.display()
        });
    }

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
    let proposed_suite = if proposed {
        format!(" {series}-proposed")
    } else {
        String::new()
    };
    sources.push_str(&formatdoc! {
        "
        Types: deb deb-src
        URIs: {archive}
        Suites: {series} {series}-updates{proposed_suite}
        Components: main universe
        Architectures: {architecture}
        Targets: Packages Sources
        Signed-By: {UBUNTU_KEYRING}

        Types: deb deb-src
        URIs: {security}
        Suites: {series}-security
        Components: main universe
        Architectures: {architecture}
        Targets: Packages Sources
        Signed-By: {UBUNTU_KEYRING}
        "
    });
    let source_path = cache.join("sources.sources");
    let previous = match fs::read(&source_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error).context("read cached APT sources"),
    };

    // Only rewrite the sources file if it has changed, to avoid an updated
    // modification time which would trigger `apt` to rebuild its package cache.
    if previous != sources.as_bytes() {
        write_file(&source_path, sources.as_bytes(), None).context("write cached APT sources")?;
    }

    Ok(AptView {
        root: cache,
        _lock: lock,
        architecture: architecture.to_owned(),
    })
}

/// Returns Ubucargo's user-writable APT cache directory.
fn cache_root() -> Result<PathBuf> {
    if let Some(path) = env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(path).join("ubucargo/apt"));
    }
    let home = env::var_os("HOME").context("neither XDG_CACHE_HOME nor HOME is set")?;
    Ok(PathBuf::from(home).join(".cache/ubucargo/apt"))
}

/// Reads the current Ubuntu development series from installed distro-info data.
pub fn read_development_series() -> Result<String> {
    let output = run_command(
        Command::new("ubuntu-distro-info").arg("--devel"),
        "ubuntu-distro-info --devel",
    )
    .context("cannot determine the Ubuntu development series; supply --series explicitly")?;
    let series = String::from_utf8(output.stdout)
        .context("invalid development-series output; supply --series explicitly")?;
    let series = series.trim();
    validate_name("development series", series)
        .context("cannot determine the Ubuntu development series; supply --series explicitly")?;
    Ok(series.to_owned())
}

/// Reads the host's native Debian architecture.
pub fn read_architecture() -> Result<String> {
    let output = run_command(
        Command::new("dpkg").arg("--print-architecture"),
        "dpkg --print-architecture",
    )?;
    let architecture = String::from_utf8(output.stdout)?.trim().to_owned();
    if architecture.is_empty() {
        bail!("dpkg --print-architecture returned no architecture");
    }
    Ok(architecture)
}

/// Validates a value inserted into an APT source stanza.
fn validate_name(kind: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-'))
    {
        bail!("invalid {kind} {value:?}");
    }
    Ok(())
}

/// Parses the documented `ppa:OWNER/NAME` syntax.
fn parse_ppa(ppa: &str) -> Result<(&str, &str)> {
    let value = ppa
        .strip_prefix("ppa:")
        .with_context(|| format!("invalid PPA {ppa:?}; expected ppa:OWNER/NAME"))?;
    let (owner, name) = value
        .split_once('/')
        .with_context(|| format!("invalid PPA {ppa:?}; expected ppa:OWNER/NAME"))?;
    if name.contains('/') {
        bail!("invalid PPA {ppa:?}; expected ppa:OWNER/NAME");
    }
    validate_name("PPA owner", owner)?;
    validate_name("PPA name", name)?;
    Ok((owner, name))
}

/// Retrieves, validates, and caches the signing key for one public PPA.
fn get_ppa_key(owner: &str, name: &str, key_directory: &Path) -> Result<PathBuf> {
    let api = format!("https://api.launchpad.net/devel/~{owner}/+archive/ubuntu/{name}");
    let output = run_command(
        Command::new("curl").args(["--fail", "--silent", "--show-error", "--location", &api]),
        &format!("query Launchpad for ppa:{owner}/{name}"),
    )?;
    let archive: LaunchpadArchive = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("parse Launchpad metadata for ppa:{owner}/{name}"))?;
    if archive.private {
        bail!("private PPA ppa:{owner}/{name} is not supported");
    }
    let fingerprint = archive
        .signing_key_fingerprint
        .context("PPA has no signing key fingerprint")?
        .to_ascii_uppercase();
    if fingerprint.len() != 40 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Launchpad returned invalid signing key fingerprint {fingerprint:?}");
    }
    let destination = key_directory.join(format!("{fingerprint}.asc"));
    if destination.is_file() {
        verify_key(&fs::read(&destination)?, &fingerprint)?;
        return Ok(destination);
    }

    let url = format!("https://keyserver.ubuntu.com/pks/lookup?op=get&search=0x{fingerprint}");
    let output = run_command(
        Command::new("curl").args(["--fail", "--silent", "--show-error", "--location", &url]),
        &format!("download PPA signing key {fingerprint}"),
    )?;
    verify_key(&output.stdout, &fingerprint)?;
    write_file(&destination, &output.stdout, None)
        .with_context(|| format!("cache PPA signing key at {}", destination.display()))?;
    Ok(destination)
}

/// Requires an armored key to contain the fingerprint advertised by Launchpad.
fn verify_key(contents: &[u8], fingerprint: &str) -> Result<()> {
    let home = tempfile::tempdir().context("create temporary GnuPG home")?;
    let mut child = Command::new("gpg")
        .args(["--batch", "--no-options", "--homedir"])
        .arg(home.path())
        .args(["--show-keys", "--with-colons"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run gpg --show-keys")?;
    child.stdin.take().unwrap().write_all(contents)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "could not inspect PPA signing key:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let listing = String::from_utf8(output.stdout)?;
    let mut public_keys = 0;
    let mut primary_fingerprint = None;
    for line in listing.lines() {
        let fields: Vec<_> = line.split(':').collect();
        if fields.first() == Some(&"pub") {
            public_keys += 1;
        } else if fields.first() == Some(&"fpr") && primary_fingerprint.is_none() {
            primary_fingerprint = fields.get(9).copied();
        }
    }
    if public_keys != 1 || primary_fingerprint != Some(fingerprint) {
        bail!("downloaded PPA key does not match fingerprint {fingerprint}");
    }
    Ok(())
}

/// Reads all binary package paragraphs from one APT index.
fn read_index(
    path: &Path,
    location: &str,
    candidates: &mut Vec<PackageCandidate>,
    candidate_indexes: &mut BTreeMap<(String, Version, String), usize>,
) -> Result<()> {
    let file =
        fs::File::open(path).with_context(|| format!("read APT index {}", path.display()))?;
    for paragraph in Deb822::iter_paragraphs_from_reader(BufReader::new(file)) {
        let paragraph = paragraph?;
        if paragraph
            .get("Package")
            .is_some_and(|name| name.starts_with("librust-"))
        {
            let package = Package::from_paragraph(&paragraph).map_err(anyhow::Error::msg)?;
            add_package(package, location, candidates, candidate_indexes)?;
        }
    }
    Ok(())
}

/// Reads source publications and authenticated descriptor checksums from an APT Sources index.
fn read_sources(path: &Path, location: &str, sources: &mut Vec<SourceCandidate>) -> Result<()> {
    let file =
        fs::File::open(path).with_context(|| format!("read source index {}", path.display()))?;
    for paragraph in Deb822::iter_paragraphs_from_reader(BufReader::new(file)) {
        let paragraph = paragraph?;
        let source = paragraph
            .get("Package")
            .context("source record has no Package")?
            .to_owned();
        let version = paragraph
            .get("Version")
            .context("source record has no Version")?
            .parse()?;
        let (checksums, checksum_algorithm, digest_length) =
            if let Some(checksums) = paragraph.get("Checksums-Sha512") {
                (checksums, ChecksumAlgorithm::Sha512, 128)
            } else {
                (
                    paragraph.get("Checksums-Sha256").with_context(|| {
                        format!("source {source} in {location} has no SHA512 or SHA256 checksums")
                    })?,
                    ChecksumAlgorithm::Sha256,
                    64,
                )
            };
        let mut descriptor = None;
        for line in checksums.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            if let [hash, size, filename] = fields.as_slice() {
                if filename.ends_with(".dsc") {
                    if filename.contains('/')
                        || hash.len() != digest_length
                        || !hash.bytes().all(|c| c.is_ascii_hexdigit())
                    {
                        bail!("invalid source descriptor metadata");
                    }
                    descriptor = Some((
                        filename.to_string(),
                        hash.to_ascii_lowercase(),
                        size.parse()?,
                    ));
                }
            }
        }
        let (dsc, checksum, size) = descriptor.with_context(|| {
            format!("source {source} in {location} has no .dsc in its selected checksum field")
        })?;
        sources.push(SourceCandidate {
            source,
            version,
            location: location.to_owned(),
            dsc,
            checksum,
            checksum_algorithm,
            size,
        });
    }
    Ok(())
}

/// Selects an exact source in the input origin, using Debian ordering and deterministic ties.
pub fn select_source<'a>(
    sources: &'a [SourceCandidate],
    name: &str,
    ppa: Option<&str>,
    version: Option<&str>,
) -> Result<&'a SourceCandidate> {
    let requested = version.map(str::parse::<Version>).transpose()?;
    let mut selected: Option<&SourceCandidate> = None;
    for source in sources {
        let origin_matches = match ppa {
            Some(ppa) => source.location.starts_with(&format!("{ppa} (")),
            None => !source.location.starts_with("ppa:"),
        };
        if source.source != name
            || !origin_matches
            || requested.as_ref().is_some_and(|v| v != &source.version)
        {
            continue;
        }
        if selected.is_none_or(|old| {
            source.version > old.version
                || (source.version == old.version && source.location < old.location)
        }) {
            selected = Some(source);
        }
    }
    selected.with_context(|| {
        format!(
            "source {name} {}is absent from the selected repositories and components",
            version.map(|v| format!("{v} ")).unwrap_or_default()
        )
    })
}

/// Downloads an indexed source, checks its descriptor, and extracts maintained packaging.
pub fn retrieve_source(
    source: &SourceCandidate,
    ppa: Option<&str>,
    keep: bool,
) -> Result<(tempfile::TempDir, PathBuf)> {
    let stage = tempfile::Builder::new().disable_cleanup(keep).tempdir()?;
    if keep {
        eprintln!(
            "published input staging directory: {}",
            stage.path().display()
        );
    }
    let mut command = Command::new(if ppa.is_some() {
        "pull-ppa-source"
    } else {
        "pull-lp-source"
    });
    command.args(["--no-conf", "--download-only", "--no-verify-signature"]);
    if let Some(ppa) = ppa {
        command.args([
            "--ppa",
            ppa.strip_prefix("ppa:")
                .context("expected ppa:OWNER/NAME")?,
        ]);
    }
    command
        .arg(&source.source)
        .arg(source.version.to_string())
        .current_dir(stage.path());
    // Authentication comes from the signed Sources checksum; uploader keys need not be installed.
    run_command(&mut command, "download indexed source package")?;
    let descriptor = stage.path().join(&source.dsc);
    verify_descriptor(&descriptor, source)?;
    let root = stage.path().join("source");
    run_command(
        Command::new("dpkg-source")
            .arg("-x")
            .arg(&descriptor)
            .arg(&root),
        "extract published source",
    )?;
    Ok((stage, root))
}

/// Checks a downloaded descriptor against authenticated source-index size and its strong digest.
fn verify_descriptor(descriptor: &Path, source: &SourceCandidate) -> Result<()> {
    if fs::metadata(&descriptor)?.len() != source.size {
        bail!("source descriptor size mismatch");
    }
    let command = match source.checksum_algorithm {
        ChecksumAlgorithm::Sha512 => "sha512sum",
        ChecksumAlgorithm::Sha256 => "sha256sum",
    };
    let output = run_command(
        Command::new(command).arg(descriptor),
        "verify source descriptor checksum",
    )?;
    if String::from_utf8(output.stdout)?.split_whitespace().next() != Some(source.checksum.as_str())
    {
        bail!("source descriptor checksum mismatch");
    }
    Ok(())
}

/// Adds one Rust binary package, rejecting non-equality version constraints in Provides.
fn add_package(
    package: Package,
    location: &str,
    candidates: &mut Vec<PackageCandidate>,
    candidate_indexes: &mut BTreeMap<(String, Version, String), usize>,
) -> Result<()> {
    let (source, source_version) = match package.source {
        Some(source) => (
            source.name,
            source.version.unwrap_or_else(|| package.version.clone()),
        ),
        None => (package.name.clone(), package.version.clone()),
    };
    let key = (source.clone(), source_version.clone(), location.to_owned());
    let index = if let Some(index) = candidate_indexes.get(&key) {
        *index
    } else {
        let index = candidates.len();
        candidates.push(PackageCandidate {
            source,
            version: source_version,
            provides: BTreeMap::new(),
            location: location.to_owned(),
        });
        candidate_indexes.insert(key, index);
        index
    };
    let candidate = &mut candidates[index];
    candidate
        .provides
        .insert(package.name, Some(package.version.clone()));
    if let Some(relations) = package.provides {
        for entry in relations.0 {
            for relation in entry {
                let version = match relation.version {
                    None => None,
                    Some((VersionConstraint::Equal, version)) => Some(version),
                    Some((constraint, version)) => bail!(
                        "invalid Provides for {} in {location}: expected '=', got {constraint} {version}",
                        relation.name
                    ),
                };
                candidate.provides.insert(relation.name, version);
            }
        }
    }
    Ok(())
}

/// Formats repository metadata as the documented compact location.
fn format_location(site: &str, release: &str, component: &str) -> String {
    if let Some(path) = site
        .strip_prefix("https://ppa.launchpadcontent.net/")
        .or_else(|| site.strip_prefix("http://ppa.launchpadcontent.net/"))
        && let Some(path) = path.strip_suffix("/ubuntu")
    {
        return format!("ppa:{path} ({release})");
    }
    format!("{release}/{component}")
}

#[cfg(test)]
mod tests {
    use indoc::{indoc, writedoc};
    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    /// Parses public PPA shorthand without accepting extra path components.
    fn parses_ppa_names() {
        assert_eq!(parse_ppa("ppa:example/rust").unwrap(), ("example", "rust"));
        assert!(parse_ppa("example/rust").is_err());
        assert!(parse_ppa("ppa:example/rust/extra").is_err());
    }

    #[test]
    /// Formats Archive and PPA index locations compactly.
    fn formats_locations() {
        assert_eq!(
            format_location(
                "https://archive.ubuntu.com/ubuntu",
                "noble-updates",
                "universe"
            ),
            "noble-updates/universe"
        );
        assert_eq!(
            format_location(
                "https://ppa.launchpadcontent.net/example/rust-staging/ubuntu",
                "noble",
                "main"
            ),
            "ppa:example/rust-staging (noble)"
        );
    }

    #[test]
    /// Holds the shared lock until the view is dropped.
    fn locks_persistent_view() {
        let cache = tempfile::tempdir().unwrap();
        let view = prepare_view(cache.path(), "noble", "amd64", false, &[]).unwrap();
        let competing = fs::File::options()
            .write(true)
            .open(cache.path().join("view.lock"))
            .unwrap();
        assert!(matches!(
            competing.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(view);
        competing.try_lock().unwrap();
    }

    #[test]
    /// Preserves timestamps and the binary cache when the selection is unchanged.
    fn preserves_unchanged_view() {
        let cache = tempfile::tempdir().unwrap();
        let normal = prepare_view(cache.path(), "noble", "amd64", false, &[]).unwrap();
        let sources = cache.path().join("sources.sources");
        let status = cache.path().join("status");
        let binary = cache.path().join("pkgcache.bin");
        let timestamp = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        for path in [&sources, &status] {
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(timestamp)
                .unwrap();
        }
        // Sentinel contents: this checks file preservation without invoking APT.
        fs::write(&binary, "cached").unwrap();
        drop(normal);

        let _unchanged = prepare_view(cache.path(), "noble", "amd64", false, &[]).unwrap();
        for path in [&sources, &status] {
            assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), timestamp);
        }
        assert_eq!(fs::read(&binary).unwrap(), b"cached");
    }

    #[test]
    /// Updates repository selections while leaving binary-cache validation to APT.
    fn updates_source_selection() {
        let cache = tempfile::tempdir().unwrap();
        let sources = cache.path().join("sources.sources");
        let binary = cache.path().join("pkgcache.bin");
        fs::write(&binary, "cached").unwrap();
        for (series, architecture, proposed) in [
            ("noble", "amd64", false),
            ("noble", "amd64", true),
            ("noble", "arm64", true),
            ("stonking", "arm64", true),
            ("noble", "amd64", false),
        ] {
            let _view = prepare_view(cache.path(), series, architecture, proposed, &[]).unwrap();
            assert_eq!(fs::read(&binary).unwrap(), b"cached");
            let contents = fs::read_to_string(&sources).unwrap();
            assert!(contents.contains(&format!("Suites: {series} {series}-updates")));
            assert!(contents.contains(&format!("Architectures: {architecture}\n")));
            assert!(contents.contains("Types: deb deb-src\n"));
            assert!(contents.contains("Targets: Packages Sources\n"));
            assert_eq!(contents.contains(&format!("{series}-proposed")), proposed);
        }
    }

    #[test]
    /// Parses versioned virtual packages from an APT Packages paragraph.
    fn parses_package_candidates() {
        let base = indoc! {r"
            Package: librust-serde-dev
            Source: rust-serde
            Version: 1.0.219-1
            Architecture: amd64
            Provides: librust-serde-1+derive-dev (= 1.0.219-1), librust-serde-dev-unversioned
        "};
        let feature = indoc! {r"
            Package: librust-serde+std-dev
            Source: rust-serde
            Version: 1.0.219-1
            Architecture: amd64
            Provides: librust-serde-1+std-dev (= 1.0.219-1)
        "};
        let mut packages = NamedTempFile::new().unwrap();
        writedoc! {packages, r"
            {base}
            {feature}"
        }
        .unwrap();
        let mut candidates = Vec::new();
        let mut indexes = BTreeMap::new();
        read_index(
            packages.path(),
            "noble/universe",
            &mut candidates,
            &mut indexes,
        )
        .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source, "rust-serde");
        assert_eq!(
            candidates[0].provides["librust-serde-1+derive-dev"]
                .as_ref()
                .unwrap()
                .to_string(),
            "1.0.219-1"
        );
        assert!(candidates[0].provides["librust-serde-1+std-dev"].is_some());
        assert_eq!(
            candidates[0].provides["librust-serde-dev-unversioned"],
            None
        );
    }
    #[test]
    /// Reads source-only publications and selects within the exact origin using Debian ordering.
    fn selects_indexed_sources() {
        let mut index = NamedTempFile::new().unwrap();
        for (name, version, binaries) in [
            ("rust-example", "1.9-1", "librust-example-dev, example"),
            ("rust-example", "1.10-1", "librust-example-dev"),
            ("rust-example-2", "2.0-1", "librust-example-2-dev"),
            ("only-source", "1.0", "only-source"),
        ] {
            writeln!(index, "Package: {name}\nVersion: {version}\nBinary: {binaries}\nChecksums-Sha256:\n {} 4 {name}.dsc\n", "a".repeat(64)).unwrap();
        }
        let mut sources = Vec::new();
        read_sources(index.path(), "noble/universe", &mut sources).unwrap();
        assert_eq!(sources.len(), 4);
        let mut ppa = sources[1].clone();
        ppa.version = "9.0-1".parse().unwrap();
        ppa.location = "ppa:owner/staging (noble)".to_owned();
        sources.push(ppa);
        let mut updates = sources[1].clone();
        updates.location = "noble-updates/universe".to_owned();
        sources.push(updates);
        let selected = select_source(&sources, "rust-example", None, None).unwrap();
        assert_eq!(selected.version.to_string(), "1.10-1");
        assert_eq!(selected.location, "noble-updates/universe");
        assert_eq!(
            select_source(&sources, "rust-example", None, Some("1.9-1"))
                .unwrap()
                .version
                .to_string(),
            "1.9-1"
        );
        assert_eq!(
            select_source(&sources, "rust-example", Some("ppa:owner/staging"), None)
                .unwrap()
                .version
                .to_string(),
            "9.0-1"
        );
        assert!(select_source(&sources, "rust-example", None, Some("9.0-1")).is_err());
        assert!(select_source(&sources, "rust-example", Some("ppa:other/staging"), None).is_err());
        assert!(select_source(&sources, "absent", None, None).is_err());
        assert!(select_source(&sources, "only-source", None, None).is_ok());
        assert_eq!(
            select_source(&sources, "rust-example-2", None, None)
                .unwrap()
                .version
                .to_string(),
            "2.0-1"
        );
    }

    #[test]
    /// Rejects corrupted descriptors before extraction, including same-size content changes.
    fn verifies_indexed_descriptor() {
        let descriptor = NamedTempFile::new().unwrap();
        fs::write(descriptor.path(), "test").unwrap();
        let mut source = SourceCandidate {
            source: "rust-example".to_owned(),
            version: "1.0".parse().unwrap(),
            location: "noble/universe".to_owned(),
            dsc: "rust-example_1.0.dsc".to_owned(),
            size: 4,
            checksum_algorithm: ChecksumAlgorithm::Sha256,
            checksum: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".to_owned(),
        };
        verify_descriptor(descriptor.path(), &source).unwrap();
        fs::write(descriptor.path(), "fail").unwrap();
        assert!(
            verify_descriptor(descriptor.path(), &source)
                .unwrap_err()
                .to_string()
                .contains("checksum mismatch")
        );
        source.checksum_algorithm = ChecksumAlgorithm::Sha512;
        source.checksum = "ee26b0dd4af7e749aa1a8ee3c10ae9923f618980772e473f8819a5d4940e0db27ac185f8a0e1d5f84f88bc887fd67b143732c304cc5fa9ad8e6f57f50028a8ff".to_owned();
        fs::write(descriptor.path(), "test").unwrap();
        verify_descriptor(descriptor.path(), &source).unwrap();
        fs::write(descriptor.path(), "fail").unwrap();
        assert!(
            verify_descriptor(descriptor.path(), &source)
                .unwrap_err()
                .to_string()
                .contains("checksum mismatch")
        );
        source.size = 5;
        assert!(
            verify_descriptor(descriptor.path(), &source)
                .unwrap_err()
                .to_string()
                .contains("size mismatch")
        );
    }
    #[test]
    /// Accepts either strong checksum field and prefers SHA512 without bypassing malformed digests.
    fn parses_strong_source_checksums() {
        let index = NamedTempFile::new().unwrap();
        for (fields, expected) in [
            (
                format!("Checksums-Sha256:\n {} 4 example.dsc\n", "a".repeat(64)),
                ChecksumAlgorithm::Sha256,
            ),
            (
                format!("Checksums-Sha512:\n {} 4 example.dsc\n", "b".repeat(128)),
                ChecksumAlgorithm::Sha512,
            ),
            (
                format!(
                    "Checksums-Sha256:\n {} 4 example.dsc\nChecksums-Sha512:\n {} 4 example.dsc\n",
                    "a".repeat(64),
                    "B".repeat(128)
                ),
                ChecksumAlgorithm::Sha512,
            ),
        ] {
            fs::write(
                index.path(),
                format!("Package: example\nVersion: 1.0\n{fields}"),
            )
            .unwrap();
            let mut sources = Vec::new();
            read_sources(index.path(), "stonking/universe", &mut sources).unwrap();
            assert_eq!(sources[0].checksum_algorithm, expected);
            if expected == ChecksumAlgorithm::Sha512 {
                assert_eq!(sources[0].checksum, "b".repeat(128));
            }
        }
        for fields in [
            "",
            "Checksums-Sha512:\n bad 4 example.dsc\n",
            "Checksums-Sha256:\n bad 4 example.dsc\n",
        ] {
            fs::write(
                index.path(),
                format!("Package: example\nVersion: 1.0\n{fields}"),
            )
            .unwrap();
            assert!(read_sources(index.path(), "stonking/universe", &mut Vec::new()).is_err());
        }
    }
}
