//! Plugin marketplaces: where plugins are found, and how one is packed and
//! listed so that someone else can install it by name.
//!
//! A marketplace is a folder holding `marketplace.json` and the packages it
//! lists, one `.zip` per plugin and platform. The folder can sit on disk (a
//! checkout of a team's repository), behind an HTTPS address (any static
//! host, a GitHub repository, a release's assets), and is read the same way.
//! Nothing here runs a plugin: installing one from a marketplace downloads
//! its package, checks it against the hash the marketplace lists, and hands
//! it to [`plugin::install_expecting`]. It still needs approval before it runs.

use crate::plugin::{self, PluginError, PluginInfo};
use crate::{settings, store, util};
use khonsu_plugin_protocol::{Manifest, Permissions, MANIFEST_FILE};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The file a marketplace folder is known by.
pub const INDEX_FILE: &str = "marketplace.json";
/// The marketplace every installation starts with.
pub const OFFICIAL: &str = "official";
/// Built from github.com/JetSquirrel/khonsugraph-marketplace, whose CI
/// uploads this index and the packages it lists.
pub const OFFICIAL_LOCATION: &str = "https://marketplace.khonsugraph.com/marketplace.json";
/// A package that runs wherever its interpreter does.
pub const ANY_TARGET: &str = "any";

/// The most an index may be.
const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const FETCH_TIMEOUT: Duration = Duration::from_secs(600);

/// `marketplace.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Index {
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default)]
    pub plugins: Vec<Entry>,
}

/// One plugin in a marketplace, at one version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// What the plugin's manifest declares, so it can be read before anything
    /// is downloaded. The installed manifest is what approval goes by.
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub packages: Vec<Package>,
}

/// One downloadable archive of a plugin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Package {
    /// `<os>-<arch>` as Rust names them (`macos-aarch64`, `windows-x86_64`),
    /// or `any`.
    pub target: String,
    /// An HTTPS address, or a path relative to the marketplace.
    pub url: String,
    /// SHA-256 of the archive, in hex.
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

/// A marketplace this installation knows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Marketplace {
    pub name: String,
    /// The address or path of its `marketplace.json`.
    pub location: String,
    #[serde(default)]
    pub added_at: u64,
}

/// `marketplaces.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Known {
    /// False once the person removed the official marketplace.
    #[serde(default = "yes")]
    official: bool,
    #[serde(default)]
    marketplaces: Vec<Marketplace>,
}

fn yes() -> bool {
    true
}

impl Default for Known {
    fn default() -> Self {
        Self {
            official: true,
            marketplaces: Vec::new(),
        }
    }
}

/// Where an installed plugin came from, for `update`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Origin {
    pub plugin: String,
    pub marketplace: String,
    pub version: String,
    pub sha256: String,
    pub at: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum MarketError {
    #[error("no marketplace named {0:?}; see `khonsu marketplace list`")]
    UnknownMarketplace(String),
    #[error("{0}")]
    Location(String),
    #[error("{location}: {message}")]
    Fetch { location: String, message: String },
    #[error("{location} is not a marketplace index: {message}")]
    BadIndex { location: String, message: String },
    #[error("no plugin {0:?} in any marketplace; see `khonsu plugin search`")]
    NoSuchPlugin(String),
    #[error("{id} is in more than one marketplace ({}); say which: {id}@<marketplace>", .marketplaces.join(", "))]
    Ambiguous { id: String, marketplaces: Vec<String> },
    #[error("{id} {version} has no package for {target} (it has: {})", .has.join(", "))]
    NoPackage {
        id: String,
        version: String,
        target: String,
        has: Vec<String>,
    },
    #[error("{url} is not the package {marketplace} lists: its SHA-256 is {actual}, and {expected} was expected. Nothing was installed")]
    Hash {
        url: String,
        marketplace: String,
        expected: String,
        actual: String,
    },
    #[error("{0}")]
    Pack(String),
    #[error("{0}")]
    Plugin(#[from] PluginError),
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// This machine's package target.
pub fn host_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn is_remote(location: &str) -> bool {
    location.starts_with("https://") || location.starts_with("http://")
}

/// The address of a marketplace's index, from what a person typed: an HTTPS
/// address (of the index or of its folder), `owner/repo` on GitHub, or a
/// folder or file on disk.
pub fn resolve_location(given: &str) -> Result<String, MarketError> {
    let given = given.trim();
    if given.is_empty() {
        return Err(MarketError::Location("a marketplace needs an address or a folder".into()));
    }
    if given.starts_with("http://") {
        return Err(MarketError::Location(format!("{given}: a marketplace is read over https, not http")));
    }
    if given.starts_with("https://") {
        let trimmed = given.trim_end_matches('/');
        return Ok(if trimmed.ends_with(".json") {
            trimmed.to_string()
        } else {
            format!("{trimmed}/{INDEX_FILE}")
        });
    }
    let path = Path::new(given);
    if path.exists() {
        let path = std::path::absolute(path)?;
        let index = if path.is_dir() { path.join(INDEX_FILE) } else { path };
        return Ok(index.display().to_string());
    }
    // owner/repo: the index at the top of a GitHub repository's default branch.
    let parts: Vec<&str> = given.split('/').collect();
    let word = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if parts.len() == 2 && parts.iter().all(|part| word(part)) && !given.starts_with('.') {
        return Ok(format!("https://raw.githubusercontent.com/{given}/HEAD/{INDEX_FILE}"));
    }
    Err(MarketError::Location(format!(
        "{given}: not a folder here, an https address, or owner/repo on GitHub"
    )))
}

/// Where a package is, given where its marketplace's index is.
fn resolve_package(index_location: &str, url: &str) -> Result<String, MarketError> {
    if url.starts_with("https://") {
        return Ok(url.to_string());
    }
    let relative_ok = !url.is_empty()
        && !url.contains("://")
        && !url.starts_with(['/', '\\'])
        && !url.contains(':')
        && !url.split(['/', '\\']).any(|piece| piece == "..");
    if !relative_ok {
        return Err(MarketError::Location(format!(
            "package address {url:?} is neither https nor a path inside the marketplace"
        )));
    }
    if is_remote(index_location) {
        let base = index_location.rsplit_once('/').map(|(base, _)| base).unwrap_or(index_location);
        return Ok(format!("{base}/{url}"));
    }
    let base = Path::new(index_location).parent().unwrap_or(Path::new("."));
    Ok(base.join(url).display().to_string())
}

/// Reads `location` into `out`, stopping past `max` bytes.
fn fetch(location: &str, max: u64, out: &mut impl Write) -> Result<u64, MarketError> {
    let failed = |message: String| MarketError::Fetch {
        location: location.to_string(),
        message,
    };
    let copy = |reader: &mut dyn Read, out: &mut dyn Write| -> Result<u64, MarketError> {
        let written = io::copy(&mut reader.take(max + 1), out).map_err(|e| failed(e.to_string()))?;
        if written > max {
            return Err(failed(format!("larger than the {max} bytes allowed")));
        }
        Ok(written)
    };
    if is_remote(location) {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(FETCH_TIMEOUT))
            .user_agent(concat!("khonsugraph/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        let mut response = agent.get(location).call().map_err(|e| failed(e.to_string()))?;
        let mut reader = response.body_mut().with_config().limit(max + 1).reader();
        copy(&mut reader, out)
    } else {
        let mut file = fs::File::open(location).map_err(|e| failed(e.to_string()))?;
        copy(&mut file, out)
    }
}

/// Reads and checks a marketplace's index.
pub fn read_index(location: &str) -> Result<Index, MarketError> {
    let mut bytes = Vec::new();
    fetch(location, MAX_INDEX_BYTES, &mut bytes)?;
    let bad = |message: String| MarketError::BadIndex {
        location: location.to_string(),
        message,
    };
    let index: Index = serde_json::from_slice(&bytes).map_err(|e| bad(e.to_string()))?;
    for entry in &index.plugins {
        if !plugin::is_valid_id(&entry.id) {
            return Err(bad(format!("plugin id {:?} must be letters, digits, '-' or '_'", entry.id)));
        }
        for package in &entry.packages {
            let hex = package.sha256.len() == 64 && package.sha256.chars().all(|c| c.is_ascii_hexdigit());
            if !hex {
                return Err(bad(format!("{} {}: sha256 must be 64 hex digits", entry.id, package.target)));
            }
        }
    }
    Ok(index)
}

fn known_path() -> PathBuf {
    settings::config_dir().join("marketplaces.json")
}

fn origins_path() -> PathBuf {
    settings::config_dir().join("installed.json")
}

/// The marketplaces to look in: the official one unless it was removed, then
/// those added, in the order they were added.
pub fn list() -> Vec<Marketplace> {
    let known: Known = store::read(&known_path()).ok().flatten().unwrap_or_default();
    let mut all = Vec::new();
    if known.official && !known.marketplaces.iter().any(|m| m.name == OFFICIAL) {
        all.push(Marketplace {
            name: OFFICIAL.into(),
            location: OFFICIAL_LOCATION.into(),
            added_at: 0,
        });
    }
    all.extend(known.marketplaces);
    all
}

/// Adds a marketplace after reading its index, under `name`, the name its
/// index gives, or one made from its address. Adding a name again replaces it.
pub fn add(given: &str, name: Option<&str>) -> Result<(Marketplace, Index), MarketError> {
    let location = resolve_location(given)?;
    let index = read_index(&location)?;
    let name = match name {
        Some(name) => name.to_string(),
        None if plugin::is_valid_id(&index.name) => index.name.clone(),
        None => name_from(&location),
    };
    if !plugin::is_valid_id(&name) {
        return Err(MarketError::Location(format!(
            "marketplace name {name:?} must be letters, digits, '-' or '_'; pass --name"
        )));
    }
    let marketplace = Marketplace {
        name,
        location,
        added_at: util::unix_now(),
    };
    let added = marketplace.clone();
    store::update(&known_path(), |known: &mut Known| {
        known.marketplaces.retain(|m| m.name != added.name);
        known.marketplaces.push(added.clone());
        Ok::<_, io::Error>(())
    })?;
    Ok((marketplace, index))
}

/// A name for a marketplace whose index gives none: the folder or repository
/// that holds it.
fn name_from(location: &str) -> String {
    let parts: Vec<&str> = location.split(['/', '\\']).filter(|part| !part.is_empty()).collect();
    let folder = parts
        .iter()
        .rev()
        .skip(1)
        .find(|part| !["HEAD", "main", "master", "download", "latest", "releases"].contains(part))
        .copied()
        .unwrap_or("marketplace");
    folder
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect()
}

/// Forgets a marketplace. Plugins installed from it stay.
pub fn remove(name: &str) -> Result<(), MarketError> {
    if !list().iter().any(|m| m.name == name) {
        return Err(MarketError::UnknownMarketplace(name.to_string()));
    }
    store::update(&known_path(), |known: &mut Known| {
        let before = known.marketplaces.len();
        known.marketplaces.retain(|m| m.name != name);
        if name == OFFICIAL && before == known.marketplaces.len() {
            known.official = false;
        }
        Ok::<_, io::Error>(())
    })?;
    Ok(())
}

/// Brings the official marketplace back, at its own address.
pub fn restore_official() -> Result<(), MarketError> {
    store::update(&known_path(), |known: &mut Known| {
        known.marketplaces.retain(|m| m.name != OFFICIAL);
        known.official = true;
        Ok::<_, io::Error>(())
    })?;
    Ok(())
}

pub fn origins() -> Vec<Origin> {
    store::read(&origins_path()).ok().flatten().unwrap_or_default()
}

/// Drops the record of where `plugin` came from: it was uninstalled, or
/// installed again from a folder.
pub fn forget_origin(plugin: &str) -> io::Result<()> {
    if !origins().iter().any(|o| o.plugin == plugin) {
        return Ok(());
    }
    store::update(&origins_path(), |origins: &mut Vec<Origin>| {
        origins.retain(|o| o.plugin != plugin);
        Ok::<_, io::Error>(())
    })
    .map(|_| ())
}

/// One plugin as a marketplace offers it, beside what is installed here.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Listing {
    pub marketplace: String,
    #[serde(flatten)]
    pub entry: Entry,
    /// The package this machine would get; `None` when there is none for it.
    pub target: Option<String>,
    /// The version installed here, from any origin.
    pub installed: Option<String>,
    /// Whether the marketplace's version is newer than the one installed.
    pub update: bool,
}

/// A marketplace that could not be read while listing.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Unreachable {
    pub marketplace: String,
    pub location: String,
    pub message: String,
}

/// The package of `entry` for this machine: its own target, else `any`.
fn package_for<'a>(entry: &'a Entry, target: &str) -> Option<&'a Package> {
    entry
        .packages
        .iter()
        .find(|p| p.target == target)
        .or_else(|| entry.packages.iter().find(|p| p.target == ANY_TARGET))
}

/// Whether version `a` is newer than `b`: by their dotted numbers, and where
/// either isn't made of numbers, whether they differ.
pub fn newer(a: &str, b: &str) -> bool {
    let numbers = |v: &str| -> Option<Vec<u64>> { v.trim_start_matches('v').split('.').map(|part| part.parse().ok()).collect() };
    match (numbers(a), numbers(b)) {
        (Some(a), Some(b)) => a > b,
        _ => a != b,
    }
}

/// Everything the known marketplaces offer (or only `only`), matching `query`
/// in its ID, name or description. A marketplace that can't be read is
/// reported and the rest are still listed.
pub fn available(query: Option<&str>, only: Option<&str>) -> Result<(Vec<Listing>, Vec<Unreachable>), MarketError> {
    let marketplaces = list();
    if let Some(only) = only {
        if !marketplaces.iter().any(|m| m.name == only) {
            return Err(MarketError::UnknownMarketplace(only.to_string()));
        }
    }
    let installed = plugin::list();
    let target = host_target();
    let needle = query.map(str::to_lowercase).filter(|q| !q.is_empty());
    let mut listings = Vec::new();
    let mut unreachable = Vec::new();
    for marketplace in marketplaces.iter().filter(|m| only.is_none_or(|only| m.name == only)) {
        let index = match read_index(&marketplace.location) {
            Ok(index) => index,
            Err(error) => {
                unreachable.push(Unreachable {
                    marketplace: marketplace.name.clone(),
                    location: marketplace.location.clone(),
                    message: error.to_string(),
                });
                continue;
            }
        };
        for entry in index.plugins {
            let matches = needle.as_ref().is_none_or(|needle| {
                [&entry.id, &entry.name, &entry.description].iter().any(|text| text.to_lowercase().contains(needle))
            });
            if !matches {
                continue;
            }
            let here = installed.iter().find(|p| p.id() == entry.id).map(|p| p.manifest.version.clone());
            listings.push(Listing {
                marketplace: marketplace.name.clone(),
                target: package_for(&entry, &target).map(|p| p.target.clone()),
                update: here.as_ref().is_some_and(|here| newer(&entry.version, here)),
                installed: here,
                entry,
            });
        }
    }
    Ok((listings, unreachable))
}

/// Finds `spec`, an ID or `id@marketplace`, in the known marketplaces.
fn find(spec: &str) -> Result<(Marketplace, Entry), MarketError> {
    let (id, wanted) = match spec.split_once('@') {
        Some((id, marketplace)) => (id, Some(marketplace)),
        None => (spec, None),
    };
    let marketplaces = list();
    if let Some(wanted) = wanted {
        let marketplace = marketplaces
            .into_iter()
            .find(|m| m.name == wanted)
            .ok_or_else(|| MarketError::UnknownMarketplace(wanted.to_string()))?;
        let entry = read_index(&marketplace.location)?
            .plugins
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| MarketError::NoSuchPlugin(spec.to_string()))?;
        return Ok((marketplace, entry));
    }
    let mut found = Vec::new();
    let mut first_error = None;
    for marketplace in marketplaces {
        match read_index(&marketplace.location) {
            Ok(index) => found.extend(index.plugins.into_iter().filter(|e| e.id == id).map(|e| (marketplace.clone(), e))),
            Err(error) => first_error = first_error.or(Some(error)),
        }
    }
    match found.len() {
        // Not found, and somewhere it could have been was not read: say that.
        0 => Err(first_error.unwrap_or_else(|| MarketError::NoSuchPlugin(id.to_string()))),
        1 => Ok(found.remove(0)),
        _ => Err(MarketError::Ambiguous {
            id: id.to_string(),
            marketplaces: found.into_iter().map(|(m, _)| m.name).collect(),
        }),
    }
}

/// What an install from a marketplace did.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Installed {
    pub plugin: PluginInfo,
    pub marketplace: String,
    pub target: String,
    pub sha256: String,
    /// The version that was installed before, if any.
    pub replaced: Option<String>,
}

/// Downloads `spec` (an ID or `id@marketplace`), checks it against the hash
/// its marketplace lists, and installs it. It is not approved by this.
pub fn install(spec: &str) -> Result<Installed, MarketError> {
    let (marketplace, entry) = find(spec)?;
    install_entry(&marketplace, &entry)
}

fn install_entry(marketplace: &Marketplace, entry: &Entry) -> Result<Installed, MarketError> {
    if plugin::BUILTIN_PLUGINS.contains(&entry.id.as_str()) {
        return Err(MarketError::Plugin(PluginError::Other(format!("{} is a built-in plugin ID", entry.id))));
    }
    let target = host_target();
    let package = package_for(entry, &target).ok_or_else(|| MarketError::NoPackage {
        id: entry.id.clone(),
        version: entry.version.clone(),
        target: target.clone(),
        has: entry.packages.iter().map(|p| p.target.clone()).collect(),
    })?;
    let url = resolve_package(&marketplace.location, &package.url)?;
    let replaced = plugin::list().into_iter().find(|p| p.id() == entry.id && !p.builtin).map(|p| p.manifest.version);

    let plugins = settings::plugins_dir();
    fs::create_dir_all(&plugins)?;
    let download = plugins.join(format!(".download-{}.zip", std::process::id()));
    let result = (|| {
        let mut file = fs::File::create(&download)?;
        fetch(&url, plugin::MAX_ARCHIVE_BYTES, &mut file)?;
        file.flush()?;
        drop(file);
        let actual = hash_file(&download)?;
        if !actual.eq_ignore_ascii_case(&package.sha256) {
            return Err(MarketError::Hash {
                url: url.clone(),
                marketplace: marketplace.name.clone(),
                expected: package.sha256.to_lowercase(),
                actual,
            });
        }
        let info = plugin::install_expecting(&download, Some((&entry.id, &entry.version)))?;
        Ok((info, actual))
    })();
    let _ = fs::remove_file(&download);
    let (info, sha256) = result?;
    let origin = Origin {
        plugin: entry.id.clone(),
        marketplace: marketplace.name.clone(),
        version: entry.version.clone(),
        sha256: sha256.clone(),
        at: util::unix_now(),
    };
    store::update(&origins_path(), |origins: &mut Vec<Origin>| {
        origins.retain(|o| o.plugin != origin.plugin);
        origins.push(origin.clone());
        Ok::<_, io::Error>(())
    })?;
    Ok(Installed {
        plugin: info,
        marketplace: marketplace.name.clone(),
        target: package.target.clone(),
        sha256,
        replaced,
    })
}

/// What `update` found for one installed plugin.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Update {
    Updated {
        #[serde(flatten)]
        installed: Box<Installed>,
    },
    Current {
        plugin: String,
        version: String,
        marketplace: String,
    },
    /// Installed from a folder or archive, so there is nowhere to look.
    NoOrigin {
        plugin: String,
    },
    Failed {
        plugin: String,
        message: String,
    },
}

/// Brings installed plugins up to what their marketplaces list now: `only`,
/// or every plugin that came from a marketplace. With `dry_run`, says what
/// would change and installs nothing.
pub fn update(only: Option<&str>, dry_run: bool) -> Result<Vec<Update>, MarketError> {
    let origins = origins();
    let ids: Vec<String> = match only {
        Some(id) => {
            plugin::find(id)?;
            vec![id.to_string()]
        }
        None => origins.iter().map(|o| o.plugin.clone()).collect(),
    };
    let mut results = Vec::new();
    for id in ids {
        let Some(origin) = origins.iter().find(|o| o.plugin == id) else {
            results.push(Update::NoOrigin { plugin: id });
            continue;
        };
        let here = plugin::find(&id).map(|p| p.manifest.version).unwrap_or_default();
        let step = find(&format!("{id}@{}", origin.marketplace)).and_then(|(marketplace, entry)| {
            if !newer(&entry.version, &here) {
                return Ok(Update::Current {
                    plugin: id.clone(),
                    version: here.clone(),
                    marketplace: marketplace.name,
                });
            }
            if dry_run {
                let target = host_target();
                let package = package_for(&entry, &target).ok_or_else(|| MarketError::NoPackage {
                    id: entry.id.clone(),
                    version: entry.version.clone(),
                    target: target.clone(),
                    has: entry.packages.iter().map(|p| p.target.clone()).collect(),
                })?;
                return Ok(Update::Updated {
                    installed: Box::new(Installed {
                        plugin: PluginInfo {
                            manifest: Manifest {
                                id: entry.id.clone(),
                                name: entry.name.clone(),
                                version: entry.version.clone(),
                                protocol: khonsu_plugin_protocol::PROTOCOL_VERSION,
                                entry: Vec::new(),
                                description: entry.description.clone(),
                                permissions: entry.permissions.clone(),
                            },
                            dir: settings::plugins_dir().join(&entry.id),
                            builtin: false,
                        },
                        marketplace: marketplace.name,
                        target: package.target.clone(),
                        sha256: package.sha256.clone(),
                        replaced: Some(here.clone()),
                    }),
                });
            }
            install_entry(&marketplace, &entry).map(|installed| Update::Updated {
                installed: Box::new(installed),
            })
        });
        results.push(step.unwrap_or_else(|error| Update::Failed {
            plugin: id,
            message: error.to_string(),
        }));
    }
    Ok(results)
}

fn hash_file(path: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    io::copy(&mut fs::File::open(path)?, &mut hash)?;
    Ok(hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

/// A packed plugin: one archive, ready to list in a marketplace.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Packed {
    pub path: PathBuf,
    pub id: String,
    pub version: String,
    pub target: String,
    pub sha256: String,
    pub size: u64,
    /// Files in the archive, relative to the plugin folder.
    pub files: Vec<String>,
    #[serde(skip)]
    pub manifest: Manifest,
}

/// What stays out of a package: what tools leave in a folder, not the plugin.
fn left_out(name: &str) -> bool {
    matches!(name, ".git" | ".DS_Store" | "__pycache__" | "Thumbs.db") || name.ends_with(".pyc")
}

/// The file in a plugin folder that names what else stays out of its package.
pub const IGNORE_FILE: &str = ".khonsuignore";

/// The lines of a plugin's `.khonsuignore`: a path inside the folder (`src/`,
/// `Cargo.toml`), or `*.ext` for every file ending so. `#` starts a comment.
fn ignored(dir: &Path) -> Result<Vec<String>, MarketError> {
    let text = match fs::read_to_string(dir.join(IGNORE_FILE)) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.trim_start_matches("./").trim_end_matches('/').replace('\\', "/"))
        .collect())
}

fn is_ignored(relative: &str, name: &str, ignore: &[String]) -> bool {
    ignore.iter().any(|rule| match rule.strip_prefix('*') {
        Some(ending) => name.ends_with(ending),
        None => rule == relative,
    })
}

fn collect(dir: &Path, prefix: &str, ignore: &[String], files: &mut Vec<(String, PathBuf)>) -> Result<(), MarketError> {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if left_out(&name) || name == IGNORE_FILE || is_ignored(&format!("{prefix}{name}"), &name, ignore) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(MarketError::Pack(format!(
                "{}: a plugin folder may not hold symbolic links",
                entry.path().display()
            )));
        }
        if kind.is_dir() {
            collect(&entry.path(), &format!("{prefix}{name}/"), ignore, files)?;
        } else {
            files.push((format!("{prefix}{name}"), entry.path()));
        }
    }
    Ok(())
}

/// Packs the plugin in `dir` as `<id>-<version>-<target>.zip` in `out`.
///
/// `extra` files go at the top of the package beside the manifest: the
/// executable a build left elsewhere, such as `target/release/khonsu-plugin-x`.
/// `target` defaults to this machine's when the entry is a file in the
/// package, and to `any` when it is a program found on `PATH`.
pub fn pack(dir: &Path, out: &Path, target: Option<&str>, extra: &[PathBuf]) -> Result<Packed, MarketError> {
    let manifest = plugin::read_manifest(dir)?;
    if plugin::BUILTIN_PLUGINS.contains(&manifest.id.as_str()) {
        return Err(MarketError::Pack(format!("{} is a built-in plugin ID", manifest.id)));
    }
    if manifest.version.trim().is_empty() {
        return Err(MarketError::Pack(format!("{MANIFEST_FILE} needs a version to be packed")));
    }
    let mut files = Vec::new();
    collect(dir, "", &ignored(dir)?, &mut files)?;
    for path in extra {
        if !path.is_file() {
            return Err(MarketError::Pack(format!("{}: no such file", path.display())));
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        files.retain(|(existing, _)| *existing != name);
        files.push((name, path.clone()));
    }
    files.sort();

    // The entry: a file in the package, or a bare name left to PATH.
    let entry = manifest.entry[0].replace('\\', "/");
    let packaged = |name: &str| files.iter().any(|(file, _)| file == name);
    let has_entry = packaged(&entry) || packaged(&format!("{entry}.exe"));
    if !has_entry && entry.contains('/') {
        return Err(MarketError::Pack(format!(
            "entry {entry:?} is not in the package; build it, or add it with --bin"
        )));
    }
    let target = match target {
        Some(target) => target.to_string(),
        None if has_entry => host_target(),
        None => ANY_TARGET.to_string(),
    };
    if target != ANY_TARGET && !has_entry {
        return Err(MarketError::Pack(format!(
            "entry {entry:?} is not in the package, so there is nothing for {target} to run; add it with --bin, or pack for --target any if it is a program on PATH"
        )));
    }
    if !target.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || target.is_empty() {
        return Err(MarketError::Pack(format!("target {target:?} must be like macos-aarch64, or any")));
    }

    fs::create_dir_all(out)?;
    let path = out.join(format!("{}-{}-{target}.zip", manifest.id, manifest.version));
    let staging = out.join(format!(".pack-{}.zip", std::process::id()));
    let written = (|| -> Result<(), MarketError> {
        let mut zip = zip::ZipWriter::new(fs::File::create(&staging)?);
        for (name, source) in &files {
            // The entry, and whatever was executable where it was packed.
            let mut mode = 0o644;
            if *name == entry || extra.iter().any(|path| path == source) {
                mode = 0o755;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if fs::metadata(source)?.permissions().mode() & 0o111 != 0 {
                    mode = 0o755;
                }
            }
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(mode);
            zip.start_file(name.as_str(), options).map_err(|e| MarketError::Pack(e.to_string()))?;
            io::copy(&mut fs::File::open(source)?, &mut zip)?;
        }
        zip.finish().map_err(|e| MarketError::Pack(e.to_string()))?;
        Ok(())
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&staging);
        return Err(error);
    }
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(&staging, &path)?;
    Ok(Packed {
        sha256: hash_file(&path)?,
        size: fs::metadata(&path)?.len(),
        path,
        id: manifest.id.clone(),
        version: manifest.version.clone(),
        target,
        files: files.into_iter().map(|(name, _)| name).collect(),
        manifest,
    })
}

/// What publishing changed in a marketplace folder.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Published {
    pub marketplace: String,
    pub index: PathBuf,
    #[serde(flatten)]
    pub packed: Packed,
    /// The version this replaced, when it was a different one.
    pub replaced: Option<String>,
    /// Packages of the version replaced, now unlisted and deleted.
    pub removed: Vec<String>,
    /// Targets the marketplace now lists for this plugin.
    pub targets: Vec<String>,
}

/// Packs the plugin in `dir` into the marketplace folder `to` and lists it in
/// that folder's `marketplace.json`, which is made if it isn't there.
///
/// The same version again replaces that target's package and keeps the other
/// targets, so each platform can publish its own build. A different version
/// replaces the entry, and the older version's packages are deleted.
///
/// With `base_url`, the index points at `<base_url>/<file>` rather than at
/// the file beside it, for packages that will be hosted somewhere else.
pub fn publish(dir: &Path, to: &Path, target: Option<&str>, extra: &[PathBuf], base_url: Option<&str>) -> Result<Published, MarketError> {
    if let Some(base) = base_url {
        if !base.starts_with("https://") {
            return Err(MarketError::Location(format!("{base}: packages are fetched over https")));
        }
    }
    fs::create_dir_all(to)?;
    let index_path = to.join(INDEX_FILE);
    let packed = pack(dir, to, target, extra)?;
    let file = packed.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let package = Package {
        target: packed.target.clone(),
        url: match base_url {
            Some(base) => format!("{}/{file}", base.trim_end_matches('/')),
            None => file.clone(),
        },
        sha256: packed.sha256.clone(),
        size: packed.size,
    };
    let default_name = name_from(&index_path.display().to_string());
    let mut replaced = None;
    let mut removed = Vec::new();
    let mut targets = Vec::new();
    let ((), index) = store::update(&index_path, |index: &mut Index| {
        if index.name.is_empty() {
            index.name = default_name.clone();
        }
        let fresh = Entry {
            id: packed.id.clone(),
            name: packed.manifest.name.clone(),
            version: packed.version.clone(),
            description: packed.manifest.description.clone(),
            permissions: packed.manifest.permissions.clone(),
            packages: Vec::new(),
        };
        let at = match index.plugins.iter().position(|e| e.id == packed.id) {
            Some(at) => at,
            None => {
                index.plugins.push(fresh.clone());
                index.plugins.len() - 1
            }
        };
        let entry = &mut index.plugins[at];
        if entry.version != packed.version {
            replaced = Some(entry.version.clone());
            removed = entry.packages.iter().map(|p| p.url.clone()).filter(|url| !url.contains("://")).collect();
            entry.packages.clear();
        }
        let packages = std::mem::take(&mut entry.packages);
        *entry = fresh;
        entry.packages = packages;
        entry.packages.retain(|p| p.target != package.target);
        entry.packages.push(package.clone());
        entry.packages.sort_by(|a, b| a.target.cmp(&b.target));
        targets = entry.packages.iter().map(|p| p.target.clone()).collect();
        index.plugins.sort_by(|a, b| a.id.cmp(&b.id));
        Ok::<_, io::Error>(())
    })?;
    for old in &removed {
        if *old != file && !old.contains(['/', '\\']) {
            let _ = fs::remove_file(to.join(old));
        }
    }
    Ok(Published {
        marketplace: index.name,
        index: index_path,
        packed,
        replaced,
        removed,
        targets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_resolve_to_an_index() {
        assert_eq!(resolve_location("https://example.com/plugins/").unwrap(), "https://example.com/plugins/marketplace.json");
        assert_eq!(resolve_location("https://example.com/p/index.json").unwrap(), "https://example.com/p/index.json");
        assert_eq!(
            resolve_location("acme/khonsu-plugins").unwrap(),
            "https://raw.githubusercontent.com/acme/khonsu-plugins/HEAD/marketplace.json"
        );
        assert!(resolve_location("http://example.com/plugins").is_err());
        assert!(resolve_location("no such thing at all").is_err());
    }

    #[test]
    fn a_package_stays_inside_its_marketplace_or_is_https() {
        let index = "https://example.com/plugins/marketplace.json";
        assert_eq!(resolve_package(index, "a-1-any.zip").unwrap(), "https://example.com/plugins/a-1-any.zip");
        assert_eq!(resolve_package(index, "https://cdn.example.com/a.zip").unwrap(), "https://cdn.example.com/a.zip");
        for bad in ["../a.zip", "/etc/passwd", "http://example.com/a.zip", "file:///a.zip", "C:\\a.zip", ""] {
            assert!(resolve_package(index, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.10.0", "0.9.1"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("0.3.0", "0.3.0"));
        assert!(!newer("0.2.0", "0.3.0"));
        assert!(newer("v1.2.0", "1.1.0"));
        // Not numbers: any difference is offered.
        assert!(newer("2026-11-nightly", "0.3.0"));
    }

    #[test]
    fn a_marketplace_is_named_after_its_folder() {
        assert_eq!(name_from("https://raw.githubusercontent.com/acme/khonsu-plugins/HEAD/marketplace.json"), "khonsu-plugins");
        assert_eq!(name_from("/Users/me/team plugins/marketplace.json"), "team-plugins");
    }
}
