//! Builds an isolated APT view and reads Rust package candidates from it.

use std::{
    collections::BTreeMap,
    env, fs,
    io::{BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};

use crate::util::{run_command, run_streaming_command, write_file};
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
    /// Declared binaries used to match the packaged crate across source names.
    pub binaries: Vec<String>,
    /// Repository provenance.
    pub location: String,
    /// Name of the source descriptor.
    pub dsc: String,
    /// Exact descriptor URL in the selected repository.
    pub dsc_url: String,
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
            "$(FILENAME)|$(SITE)|$(REPO_URI)|$(RELEASE)|$(COMPONENT)|$(ARCHITECTURE)|$(IDENTIFIER)",
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
            repository_uri,
            release,
            component,
            index_architecture,
            identifier,
        ] = fields.as_slice()
        else {
            bail!(
                "unexpected apt-get indextargets row: expected 7 fields, got {}: {line:?}",
                fields.len()
            );
        };
        let location = format_location(site, release, component);
        if *identifier == "Sources" {
            read_sources(Path::new(filename), repository_uri, &location, &mut sources)?;
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

/// Queries published input metadata independently of dependency environment options.
pub fn load_source_records(
    input: &crate::input::Input,
    architecture: &str,
) -> Result<Vec<SourceCandidate>> {
    let mut ppas = Vec::new();
    let series = match input {
        crate::input::Input::Archive { suite, .. } => suite,
        crate::input::Input::Ppa { ppa, series, .. } => {
            ppas.push(ppa.clone());
            series
        }
        _ => bail!("expected a published source input"),
    };
    Ok(load_records(series, architecture, false, &ppas)?.sources)
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
    let (_, pocket) = crate::input::split_archive_suite(series);
    let suites = if pocket.is_some() {
        series.to_owned()
    } else if proposed {
        format!("{series} {series}-updates {series}-proposed")
    } else {
        format!("{series} {series}-updates")
    };
    let archive = if pocket == Some("security") {
        security
    } else {
        archive
    };
    sources.push_str(&formatdoc! {
        "
        Types: deb deb-src
        URIs: {archive}
        Suites: {suites}
        Components: main universe
        Architectures: {architecture}
        Targets: Packages Sources
        Signed-By: {UBUNTU_KEYRING}
        "
    });
    if pocket.is_none() {
        sources.push_str(&formatdoc! {
            "

            Types: deb deb-src
            URIs: {security}
            Suites: {series}-security
            Components: main universe
            Architectures: {architecture}
            Targets: Packages Sources
            Signed-By: {UBUNTU_KEYRING}
            "
        });
    }
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
fn read_sources(
    path: &Path,
    repository_uri: &str,
    location: &str,
    sources: &mut Vec<SourceCandidate>,
) -> Result<()> {
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
        let mut binaries = Vec::new();
        if let Some(names) = paragraph.get("Binary") {
            for name in names.split(',') {
                binaries.push(name.trim().to_owned());
            }
        }
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
        let directory = paragraph
            .get("Directory")
            .context("source record has no Directory")?;
        let dsc_url = format!(
            "{}/{}/{}",
            repository_uri.trim_end_matches('/'),
            directory.trim_matches('/'),
            dsc
        );
        sources.push(SourceCandidate {
            dsc_url,
            binaries,
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
    keep: bool,
) -> Result<(tempfile::TempDir, PathBuf)> {
    let stage = tempfile::Builder::new().disable_cleanup(keep).tempdir()?;
    if keep {
        eprintln!(
            "published input staging directory: {}",
            stage.path().display()
        );
    }
    // Authentication comes from the signed Sources checksum; uploader keys need not be installed.
    eprintln!("Downloading {} {} ...", source.source, source.version);
    run_streaming_command(
        Command::new("dget")
            .args([
                "--no-conf",
                "--quiet",
                "--download-only",
                "--allow-unauthenticated",
            ])
            .arg(&source.dsc_url)
            .current_dir(stage.path()),
        "download indexed source package",
    )?;
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
