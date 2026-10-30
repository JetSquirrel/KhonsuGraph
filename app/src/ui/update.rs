//! Updating KhonsuGraph from its GitHub releases, after DuckLocal's.
//!
//! The check is one request for the latest release on the GitHub API, which
//! names the release and lists each package with its size and SHA-256. It is
//! only made when asked: a click on the version in the status bar, the
//! menu's "Check for Updates…", or — once someone turns it on — at launch,
//! the `update_check` setting being `on`.
//!
//! A newer release is downloaded right away, checked against its listed
//! size and SHA-256 (and on macOS against the running app's signing team),
//! and unpacked beside the cache. It replaces the installed app when
//! KhonsuGraph quits: at once on "Restart to update", else on the next quit.
//! On Windows that is the whole folder the zip holds — the app, its
//! `khonsu.exe` copy and the official plugins — since they are released
//! together. Where the app cannot replace itself — a development build, a
//! folder it may not write, a platform with no package — the status bar
//! links to the release page instead.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use gpui_kit::{App, Global, Subscription};
use sha2::{Digest, Sha256};

const LATEST_URL: &str = "https://api.github.com/repos/JetSquirrel/KhonsuGraph/releases/latest";
const RELEASES_URL: &str = "https://github.com/JetSquirrel/KhonsuGraph/releases";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    /// Not checked since launch.
    #[default]
    Idle,
    Checking,
    UpToDate,
    /// A newer release this copy cannot install itself: the status bar
    /// links to its release page.
    Available { tag: String, url: String },
    /// A newer release being downloaded and unpacked.
    Downloading(String),
    /// A newer release unpacked and waiting for KhonsuGraph to quit.
    Ready(String),
    Failed(String),
}

#[derive(Default)]
struct Update {
    status: Status,
    /// `None` until read from the settings.
    at_launch: Option<bool>,
    /// Installs the downloaded release when KhonsuGraph quits.
    _on_quit: Option<Subscription>,
}

impl Global for Update {}

/// An unpacked release: the new app (macOS) or folder (Windows), and the
/// installed one it replaces.
#[derive(Clone)]
struct Staged {
    new: PathBuf,
    target: PathBuf,
}

/// What the GitHub API says of the latest release: its tag, and each
/// package's size, digest and download link.
#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(serde::Deserialize, Clone)]
struct Asset {
    name: String,
    size: u64,
    /// `sha256:<hex>`; GitHub computes it for every upload.
    digest: Option<String>,
    browser_download_url: String,
}

impl Asset {
    fn sha256(&self) -> Option<&str> {
        self.digest.as_deref()?.strip_prefix("sha256:")
    }
}

pub fn status(cx: &App) -> Status {
    cx.try_global::<Update>().map(|update| update.status.clone()).unwrap_or_default()
}

/// Whether the check runs at launch: only once someone has turned it on.
pub fn at_launch(cx: &mut App) -> bool {
    *cx.default_global::<Update>().at_launch.get_or_insert_with(|| {
        crate::prefs::get("update_check").ok().flatten().is_some_and(|value| value == "on")
    })
}

/// Turn the launch check on or off and remember the choice. Turning it on
/// checks now, rather than at the next launch.
pub fn toggle_at_launch(cx: &mut App) {
    let on = !at_launch(cx);
    cx.default_global::<Update>().at_launch = Some(on);
    if let Err(error) = crate::prefs::set("update_check", if on { "on" } else { "off" }) {
        tracing::warn!("update_check not saved: {error:#}");
    }
    if on {
        check(cx);
    }
}

/// The launch half: check if the setting says to.
pub fn on_launch(cx: &mut App) {
    if at_launch(cx) {
        check(cx);
    }
}

/// Ask GitHub for the latest release, off the UI thread, and download a
/// newer one; every window redraws as it goes. A check or download in
/// flight is not started twice, and a downloaded release is not fetched
/// again.
pub fn check(cx: &mut App) {
    if matches!(status(cx), Status::Checking | Status::Downloading(_) | Status::Ready(_)) {
        return;
    }
    set(Status::Checking, cx);
    cx.spawn(async move |cx| {
        let release = match smol::unblock(fetch_latest).await {
            Ok(release) => release,
            Err(error) => {
                tracing::warn!("update check failed: {error:#}");
                cx.update(|cx| set(Status::Failed(error.to_string()), cx));
                return;
            }
        };
        let tag = release.tag_name.clone();
        if !is_newer(&tag, CURRENT) {
            cx.update(|cx| set(Status::UpToDate, cx));
            return;
        }
        let asset = PACKAGE.and_then(|name| release.assets.iter().find(|asset| asset.name == name).cloned());
        let (Some(asset), Some(target)) = (asset, install_target()) else {
            let url = format!("{RELEASES_URL}/tag/{tag}");
            cx.update(|cx| set(Status::Available { tag, url }, cx));
            return;
        };
        cx.update(|cx| set(Status::Downloading(tag.clone()), cx));
        let staged = {
            let tag = tag.clone();
            smol::unblock(move || stage(&tag, &asset, target)).await
        };
        cx.update(|cx| match staged {
            Ok(staged) => {
                *staged_release() = Some(staged);
                let on_quit = cx.on_app_quit(|_| {
                    install();
                    async {}
                });
                cx.default_global::<Update>()._on_quit = Some(on_quit);
                set(Status::Ready(tag), cx);
            }
            Err(error) => {
                tracing::warn!("update download failed: {error:#}");
                set(Status::Failed(error.to_string()), cx);
            }
        });
    })
    .detach();
}

/// Quit, put the downloaded release in place, and open it.
pub fn restart(cx: &mut App) {
    if staged_release().is_some() {
        // The quit hook installs it; only this way does it reopen.
        RELAUNCH.store(true, Ordering::Relaxed);
        cx.quit();
    }
}

static RELAUNCH: AtomicBool = AtomicBool::new(false);

/// The downloaded release, once unpacked; the quit hook takes it.
fn staged_release() -> std::sync::MutexGuard<'static, Option<Staged>> {
    static STAGED: Mutex<Option<Staged>> = Mutex::new(None);
    STAGED.lock().unwrap_or_else(|error| error.into_inner())
}

fn set(status: Status, cx: &mut App) {
    cx.default_global::<Update>().status = status;
    cx.refresh_windows();
}

/// The package this build updates from, as the release names it.
const PACKAGE: Option<&str> = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("khonsugraph-macos-arm64.dmg")
} else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
    Some("khonsugraph-windows-x86_64.zip")
} else {
    None
};

/// The folder the Windows zip holds everything in.
const WINDOWS_FOLDER: &str = "khonsugraph-windows-x86_64";

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        // No overall timeout: a package is tens of megabytes on whatever
        // line the user has. Connecting and the first byte still give up.
        ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            // The GitHub API refuses a request without one.
            .user_agent(concat!("KhonsuGraph/", env!("CARGO_PKG_VERSION"), " (+https://github.com/JetSquirrel/KhonsuGraph)"))
            .build()
            .into()
    })
}

fn fetch_latest() -> anyhow::Result<Release> {
    let body = agent()
        .get(LATEST_URL)
        .header("Accept", "application/vnd.github+json")
        .call()?
        .body_mut()
        .with_config()
        .limit(1024 * 1024)
        .read_to_string()?;
    Ok(serde_json::from_str(&body)?)
}

/// What a release replaces: the `.app` bundle the executable sits in on
/// macOS, the folder the executable sits in on Windows — `None` when this
/// copy cannot replace itself. A development build never does: `cargo run`
/// would overwrite `target/` with a release.
fn install_target() -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        return None;
    }
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let target = if cfg!(target_os = "macos") {
        exe.ancestors().find(|path| path.extension().is_some_and(|ext| ext == "app"))?.to_path_buf()
    } else {
        exe.parent()?.to_path_buf()
    };
    // A downloaded app run in place from a read-only, translocated copy, or
    // one installed where only an administrator may write.
    let dir = if cfg!(target_os = "macos") { target.parent()? } else { &target };
    let probe = dir.join(".khonsugraph-update-probe");
    std::fs::File::create(&probe).ok()?;
    let _ = std::fs::remove_file(&probe);
    Some(target)
}

/// Where downloaded releases are unpacked.
fn updates_dir() -> anyhow::Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "KhonsuGraph").ok_or_else(|| anyhow::anyhow!("no cache directory"))?;
    Ok(dirs.cache_dir().join("updates"))
}

/// Download `asset` of release `tag`, check it, and unpack what replaces
/// `target`.
fn stage(tag: &str, asset: &Asset, target: PathBuf) -> anyhow::Result<Staged> {
    let dir = updates_dir()?.join(tag);
    // A half-finished earlier attempt is started over.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(&asset.name);
    download(asset, &file)?;
    let new = unpack(&file, &dir)?;
    verify(&new, &target)?;
    Ok(Staged { new, target })
}

/// Fetch `asset` into `path`, failing unless it is the listed size and hash.
fn download(asset: &Asset, path: &Path) -> anyhow::Result<()> {
    let expected = asset.sha256().ok_or_else(|| anyhow::anyhow!("the release lists no checksum for {}", asset.name))?;
    let mut response = agent().get(&asset.browser_download_url).call()?;
    let mut reader = response.body_mut().with_config().limit(asset.size + 1).reader();
    let mut out = std::fs::File::create(path)?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        out.write_all(&buffer[..read])?;
        size += read as u64;
    }
    out.sync_all()?;
    anyhow::ensure!(size == asset.size, "the download is {size} bytes, not {}", asset.size);
    let digest = format!("{:x}", hasher.finalize());
    anyhow::ensure!(digest.eq_ignore_ascii_case(expected), "the download does not match its checksum");
    Ok(())
}

/// The app (macOS) or folder (Windows) inside downloaded package `file`,
/// copied out into `dir`.
fn unpack(file: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    if cfg!(target_os = "macos") {
        let mount = dir.join("mount");
        std::fs::create_dir_all(&mount)?;
        run(std::process::Command::new("hdiutil")
            .args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"])
            .arg(&mount)
            .arg(file))?;
        let app = dir.join("KhonsuGraph.app");
        let copied = run(std::process::Command::new("ditto").arg(mount.join("KhonsuGraph.app")).arg(&app));
        let _ = run(std::process::Command::new("hdiutil").args(["detach", "-force"]).arg(&mount));
        copied?;
        Ok(app)
    } else {
        // `tar` reads the zip too on Windows 10 and later.
        let out = dir.join("unpacked");
        std::fs::create_dir_all(&out)?;
        run(std::process::Command::new("tar").arg("-xf").arg(file).arg("-C").arg(&out))?;
        let folder = out.join(WINDOWS_FOLDER);
        let exe = folder.join("khonsugraph.exe");
        anyhow::ensure!(exe.is_file(), "the package holds no {}", exe.display());
        Ok(folder)
    }
}

/// On macOS, that the new app's signature holds and is the same team's as
/// the running app's: a checksum from the same release proves no more than
/// the release. Windows builds are not signed.
fn verify(new: &Path, target: &Path) -> anyhow::Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    run(std::process::Command::new("codesign").args(["--verify", "--deep", "--strict"]).arg(new))?;
    let team = |app: &Path| -> anyhow::Result<String> {
        let output = std::process::Command::new("codesign").args(["-dv", "--verbose=2"]).arg(app).output()?;
        // codesign describes a signature on stderr.
        team_identifier(&String::from_utf8_lossy(&output.stderr))
            .ok_or_else(|| anyhow::anyhow!("{} has no signing team", app.display()))
    };
    let (new_team, current_team) = (team(new)?, team(target)?);
    anyhow::ensure!(new_team == current_team, "the new app is signed by {new_team}, not {current_team}");
    Ok(())
}

/// The `TeamIdentifier` in `codesign -dv` output; `None` for an ad-hoc or
/// missing signature.
fn team_identifier(description: &str) -> Option<String> {
    description
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|team| !team.is_empty() && *team != "not set")
        .map(str::to_string)
}

fn run(command: &mut std::process::Command) -> anyhow::Result<()> {
    let output = command.output()?;
    anyhow::ensure!(
        output.status.success(),
        "{:?} failed: {}",
        command.get_program(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

/// Put the staged release in place, and open it if `restart` asked to. Runs
/// as KhonsuGraph quits.
fn install() {
    let Some(staged) = staged_release().take() else {
        return;
    };
    if let Err(error) = install_staged(&staged, RELAUNCH.load(Ordering::Relaxed)) {
        tracing::warn!("installing the update failed: {error:#}");
    }
}

#[cfg(target_os = "macos")]
fn install_staged(staged: &Staged, relaunch: bool) -> anyhow::Result<()> {
    // The bundle cannot be swapped while it runs: a shell that outlives
    // KhonsuGraph waits for it to exit, moves the old app aside, the new one
    // in — the old one back if that fails — and opens the result.
    const SCRIPT: &str = r#"
pid=$1; new=$2; app=$3; relaunch=$4
while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done
old="$app.old-$$"
if mv "$app" "$old"; then
  if mv "$new" "$app"; then rm -rf "$old"; else mv "$old" "$app"; fi
fi
if [ "$relaunch" = 1 ]; then open "$app"; fi
"#;
    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(SCRIPT)
        .arg("khonsugraph-update")
        .arg(std::process::id().to_string())
        .arg(&staged.new)
        .arg(&staged.target)
        .arg(if relaunch { "1" } else { "0" })
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn install_staged(staged: &Staged, relaunch: bool) -> anyhow::Result<()> {
    replace_files(&staged.new, &staged.target)?;
    if relaunch {
        std::process::Command::new(staged.target.join("khonsugraph.exe")).spawn()?;
    }
    Ok(())
}

/// Copy every file under `new` to the same place under `target`. Windows
/// will not overwrite a running executable — the app itself, `khonsu.exe`
/// serving a command, a plugin mid-sync — but will rename one, so each file
/// already there is moved aside first; the next launch deletes it. Files
/// the release no longer has are left alone.
#[cfg(not(target_os = "macos"))]
fn replace_files(new: &Path, target: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(new)? {
        let entry = entry?;
        let (from, to) = (entry.path(), target.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            std::fs::create_dir_all(&to)?;
            replace_files(&from, &to)?;
            continue;
        }
        let incoming = with_suffix(&to, ".update");
        std::fs::copy(&from, &incoming)?;
        if to.exists() {
            let old = with_suffix(&to, OLD_SUFFIX);
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&to, &old)?;
        }
        std::fs::rename(&incoming, &to)?;
    }
    Ok(())
}

/// What a replaced file is renamed to until the next launch deletes it.
const OLD_SUFFIX: &str = ".update-old";

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Remove what the last update left: its download, and on Windows the files
/// it replaced, in the app's folder and its plugins'. Run at launch, before
/// a check can start another.
pub fn clean_up() {
    if let Ok(dir) = updates_dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
    if cfg!(windows) {
        if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
            remove_old_files(&dir, 2);
        }
    }
}

/// Delete the `*.update-old` files under `dir`, `depth` folders down: the
/// app's folder holds `plugins/<id>/`.
fn remove_old_files(dir: &Path, depth: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth > 0 {
                remove_old_files(&path, depth - 1);
            }
        } else if path.to_string_lossy().ends_with(OLD_SUFFIX) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Whether release `tag` is later than version `current`: numerically,
/// part by part, a `v` prefix and any `-suffix` ignored.
pub fn is_newer(tag: &str, current: &str) -> bool {
    fn parts(version: &str) -> Vec<u64> {
        let version = version.trim().trim_start_matches(['v', 'V']);
        let version = version.split(['-', '+']).next().unwrap_or(version);
        version
            .split('.')
            .map(|part| {
                let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().unwrap_or(0)
            })
            .collect()
    }
    let (mut tag, mut current) = (parts(tag), parts(current));
    let len = tag.len().max(current.len());
    tag.resize(len, 0);
    current.resize(len, 0);
    tag > current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(is_newer("v0.3.0", "0.2.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.99.1"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("0.2", "0.2.0"));
        assert!(!is_newer("v0.1.9", "0.2.0"));
        assert!(!is_newer("v0.2.0-beta.1", "0.2.0"));
    }

    #[test]
    fn the_release_reads_as_github_returns_it() {
        let release: Release = serde_json::from_str(
            r#"{"tag_name":"v0.2.0","name":"KhonsuGraph v0.2.0","assets":[
                {"name":"khonsugraph-windows-x86_64.zip","size":42,
                 "digest":"sha256:ab12","browser_download_url":"https://example/zip"},
                {"name":"old.zip","size":1,"digest":null,"browser_download_url":"https://example/old"}]}"#,
        )
        .unwrap();
        assert_eq!(release.tag_name, "v0.2.0");
        assert_eq!(release.assets[0].sha256(), Some("ab12"));
        assert_eq!(release.assets[1].sha256(), None);
    }

    #[test]
    fn only_a_real_team_signs() {
        let signed = "Identifier=app.khonsugraph\nTeamIdentifier=ABCDE12345\nSealed Resources";
        assert_eq!(team_identifier(signed).as_deref(), Some("ABCDE12345"));
        assert_eq!(team_identifier("TeamIdentifier=not set\n"), None);
        assert_eq!(team_identifier("Signature=adhoc\n"), None);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn replacing_moves_the_old_files_aside_and_clean_up_removes_them() {
        let root = tempfile::tempdir().unwrap();
        let (new, target) = (root.path().join("new"), root.path().join("app"));
        std::fs::create_dir_all(new.join("plugins/csv")).unwrap();
        std::fs::create_dir_all(target.join("plugins/csv")).unwrap();
        std::fs::write(new.join("khonsugraph.exe"), "new app").unwrap();
        std::fs::write(new.join("plugins/csv/khonsu-plugin-csv.exe"), "new plugin").unwrap();
        std::fs::write(new.join("plugins/csv/khonsu-plugin.json"), "{}").unwrap();
        std::fs::write(target.join("khonsugraph.exe"), "old app").unwrap();
        std::fs::write(target.join("plugins/csv/khonsu-plugin-csv.exe"), "old plugin").unwrap();
        std::fs::write(target.join("notes.txt"), "mine").unwrap();

        replace_files(&new, &target).unwrap();
        assert_eq!(std::fs::read_to_string(target.join("khonsugraph.exe")).unwrap(), "new app");
        assert_eq!(std::fs::read_to_string(target.join("plugins/csv/khonsu-plugin-csv.exe")).unwrap(), "new plugin");
        assert!(target.join("plugins/csv/khonsu-plugin.json").is_file());
        assert_eq!(std::fs::read_to_string(target.join("khonsugraph.exe.update-old")).unwrap(), "old app");
        assert!(target.join("plugins/csv/khonsu-plugin-csv.exe.update-old").is_file());

        remove_old_files(&target, 2);
        assert!(!target.join("khonsugraph.exe.update-old").exists());
        assert!(!target.join("plugins/csv/khonsu-plugin-csv.exe.update-old").exists());
        assert_eq!(std::fs::read_to_string(target.join("notes.txt")).unwrap(), "mine");
    }

    /// The published release downloads, matches its digest and unpacks to
    /// the folder `install` copies from. Needs the network.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn the_latest_windows_package_downloads_and_unpacks() {
        let release = fetch_latest().unwrap();
        let asset = release.assets.iter().find(|asset| Some(asset.name.as_str()) == PACKAGE).unwrap().clone();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(&asset.name);
        download(&asset, &file).unwrap();
        let folder = unpack(&file, dir.path()).unwrap();
        assert!(folder.join("khonsu.exe").is_file());
        assert!(folder.join("plugins/csv/khonsu-plugin.json").is_file());
    }
}
