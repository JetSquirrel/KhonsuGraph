//! Putting the `khonsu` command on the PATH, and telling AI agents about it,
//! after DuckLocal's "Command line & AI" dialog.
//!
//! The app's executable is the command too (see `main.rs`): on macOS and
//! Linux the command is a link named `khonsu` to it; on Windows, where a
//! command is found by file name, a copy named `khonsu.exe` beside it, whose
//! folder goes on the user's Path. Installing the app does neither, and an
//! agent that has never heard of KhonsuGraph will not go looking. Both are
//! one click from the window.
//!
//! Blocking functions; call via `smol::unblock` from UI code. On macOS the
//! install may wait on the system's administrator prompt.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _, Result};

/// The skill agents load to learn the CLI: `skills/khonsugraph` in this
/// repository, compiled in so the app can install the copy that matches it.
const SKILL: &str = include_str!("../../skills/khonsugraph/SKILL.md");
const SKILL_CLI_REFERENCE: &str = include_str!("../../skills/khonsugraph/references/cli.md");

/// Where the command stands on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandStatus {
    /// `khonsu` resolves to the copy beside this app.
    Installed(PathBuf),
    /// The install location holds something else: another copy of
    /// KhonsuGraph (an older download, a source build), or a file that is not
    /// ours. macOS and Linux only; Windows adds a folder to Path instead.
    #[cfg_attr(windows, allow(dead_code))]
    Elsewhere(PathBuf),
    NotInstalled,
}

/// What `install_command` did, for the confirmation the window shows.
#[derive(Clone, Debug)]
pub struct Installed {
    pub path: PathBuf,
    /// Linux only: `~/.local/bin` is not on the PATH, so a new shell will not
    /// find the command until it is added.
    pub needs_path: bool,
}

/// This executable, resolved through symlinks. Not on Windows, where
/// canonicalizing yields a `\\?\C:\…` path that has no business in `Path`.
fn executable() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("Cannot locate the KhonsuGraph executable")?;
    Ok(if cfg!(windows) { exe } else { exe.canonicalize().unwrap_or(exe) })
}

/// What the `khonsu` command runs: this executable, or on Windows its copy
/// `khonsu.exe` beside it (which may not exist yet; see `install_command`).
fn command() -> Result<PathBuf> {
    let exe = executable()?;
    Ok(if cfg!(windows) { exe.with_file_name("khonsu.exe") } else { exe })
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

// --- macOS and Linux: a symlink in a directory on the PATH -----------------

/// `/usr/local/bin` on macOS: on the default PATH of every shell, and where
/// `code`, `subl` and the rest put theirs. `~/.local/bin` on Linux: the XDG
/// place for a user's own commands, no root needed.
#[cfg(unix)]
fn link_path() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        Ok(PathBuf::from("/usr/local/bin/khonsu"))
    } else {
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
        Ok(PathBuf::from(home).join(".local/bin/khonsu"))
    }
}

#[cfg(unix)]
pub fn command_status() -> CommandStatus {
    let (Ok(link), Ok(cli)) = (link_path(), command()) else {
        return CommandStatus::NotInstalled;
    };
    if std::fs::symlink_metadata(&link).is_err() {
        return CommandStatus::NotInstalled;
    }
    if same_file(&link, &cli) {
        CommandStatus::Installed(link)
    } else {
        CommandStatus::Elsewhere(link)
    }
}

#[cfg(unix)]
pub fn install_command() -> Result<Installed> {
    let cli = command()?;
    let text = cli.to_string_lossy();
    // Opened straight from the disk image or a quarantined download, macOS
    // runs the app from a randomized read-only copy; a link to that breaks
    // the moment the image is ejected.
    if text.contains("/AppTranslocation/") || text.starts_with("/Volumes/") {
        bail!(crate::i18n::tr("setup.error.move_to_applications"));
    }
    let link = link_path()?;
    // Replace a link (an older copy's, most likely), never a real file: that
    // is somebody else's `khonsu`, a package manager's perhaps.
    if let Ok(meta) = std::fs::symlink_metadata(&link) {
        if !meta.file_type().is_symlink() {
            bail!(crate::i18n::trf("setup.error.not_ours", &[&link.display().to_string()]));
        }
    }
    match link_directly(&cli, &link) {
        Ok(()) => {}
        #[cfg(target_os = "macos")]
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => link_as_administrator(&cli, &link)?,
        Err(e) => return Err(e).context(format!("Cannot create {}", link.display())),
    }
    let dir = link.parent().map(Path::to_path_buf).unwrap_or_default();
    let needs_path = !cfg!(target_os = "macos") && !on_path(&dir);
    Ok(Installed { path: link, needs_path })
}

#[cfg(unix)]
fn link_directly(target: &Path, link: &Path) -> std::io::Result<()> {
    if let Some(dir) = link.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::remove_file(link) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::os::unix::fs::symlink(target, link)
}

/// `/usr/local/bin` belongs to root on a Mac without Homebrew there: ask for
/// the administrator password through the system's own prompt, the way other
/// editors install their commands. The paths travel as arguments, never
/// spliced into the script, so no quoting can break it.
#[cfg(target_os = "macos")]
fn link_as_administrator(target: &Path, link: &Path) -> Result<()> {
    let dir = link.parent().unwrap_or(Path::new("/usr/local/bin"));
    let output = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "do shell script \"mkdir -p \" & quoted form of (item 3 of argv) & \
             \" && ln -sfn \" & quoted form of (item 1 of argv) & \" \" & \
             quoted form of (item 2 of argv) with prompt \"KhonsuGraph wants to add \
             the khonsu command to your terminal.\" with administrator privileges",
            "-e",
            "end run",
        ])
        .arg(target)
        .arg(link)
        .arg(dir)
        .output()
        .context("Cannot run osascript")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // -128 is the user pressing Cancel on the password prompt.
    if stderr.contains("-128") {
        bail!(crate::i18n::tr("setup.error.cancelled"));
    }
    bail!("{}", stderr.trim())
}

#[cfg(unix)]
fn on_path(dir: &Path) -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|entry| same_file(&entry, dir)))
        .unwrap_or(false)
}

// --- Windows: the app's folder on the user's Path --------------------------

#[cfg(windows)]
pub fn command_status() -> CommandStatus {
    let Ok(cli) = command() else {
        return CommandStatus::NotInstalled;
    };
    if !cli.is_file() {
        return CommandStatus::NotInstalled;
    }
    let dir = cli.parent().map(Path::to_path_buf).unwrap_or_default();
    match user_path() {
        Ok(path) if path.split(';').any(|entry| same_file(Path::new(entry), &dir)) => CommandStatus::Installed(cli),
        _ => CommandStatus::NotInstalled,
    }
}

/// The user-scope `Path`, read from the registry rather than this process's
/// environment, which does not see a change made after it started.
#[cfg(windows)]
fn user_path() -> Result<String> {
    powershell("[Environment]::GetEnvironmentVariable('Path', 'User')", None)
}

#[cfg(windows)]
pub fn install_command() -> Result<Installed> {
    let cli = command()?;
    // The package ships the copy; a build that lacks it gets one now, and a
    // stale one (from an older app) is replaced.
    let exe = executable()?;
    if std::fs::read(&cli).ok() != std::fs::read(&exe).ok() {
        std::fs::copy(&exe, &cli).with_context(|| format!("Cannot create {}", cli.display()))?;
    }
    let dir = cli
        .parent()
        .ok_or_else(|| anyhow!("The executable has no folder"))?
        .to_string_lossy()
        .into_owned();
    // Appended to the user's Path, not the machine's: no administrator
    // needed, and new terminals see it. The folder travels in the
    // environment, never spliced into the script, so no quoting can break it.
    powershell(
        "$dir = $env:KHONSU_DIR; \
         $p = [Environment]::GetEnvironmentVariable('Path', 'User'); \
         if (-not $p) { $p = '' }; \
         if (-not (($p -split ';') -contains $dir)) { \
           [Environment]::SetEnvironmentVariable('Path', ($p.TrimEnd(';') + ';' + $dir).TrimStart(';'), 'User') \
         }",
        Some(&dir),
    )?;
    Ok(Installed { path: cli, needs_path: false })
}

#[cfg(windows)]
fn powershell(script: &str, dir: Option<&str>) -> Result<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = std::process::Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW);
    if let Some(dir) = dir {
        command.env("KHONSU_DIR", dir);
    }
    let output = command.output().context("Cannot run PowerShell")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

// --- AI agents -------------------------------------------------------------

/// `~/.claude/skills/khonsugraph`: a user-level skill, so Claude Code finds it
/// in every project, not only the one it was copied into.
fn claude_skill_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| anyhow!("Cannot find the home folder"))?;
    Ok(PathBuf::from(home).join(".claude/skills/khonsugraph"))
}

/// Whether the skill is installed, and whether it is this version's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkillStatus {
    Current(PathBuf),
    Outdated(PathBuf),
    NotInstalled,
}

pub fn claude_skill_status() -> SkillStatus {
    let Ok(dir) = claude_skill_dir() else {
        return SkillStatus::NotInstalled;
    };
    match std::fs::read_to_string(dir.join("SKILL.md")) {
        Ok(text) if text == SKILL => SkillStatus::Current(dir),
        Ok(_) => SkillStatus::Outdated(dir),
        Err(_) => SkillStatus::NotInstalled,
    }
}

/// Write the skill this build carries. Only its two files are written, so
/// anything else a user keeps in that folder survives an update.
pub fn install_claude_skill() -> Result<PathBuf> {
    install_skill_into(&claude_skill_dir()?)
}

fn install_skill_into(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir.join("references")).with_context(|| format!("Cannot create {}", dir.display()))?;
    std::fs::write(dir.join("SKILL.md"), SKILL)?;
    std::fs::write(dir.join("references/cli.md"), SKILL_CLI_REFERENCE)?;
    Ok(dir.to_path_buf())
}

/// A brief to paste into any other agent: what the command is, where it is,
/// and the habits that keep its answers honest. The full path goes in too,
/// so it works even before the command is on the PATH.
pub fn agent_prompt() -> String {
    let command = match command_status() {
        CommandStatus::Installed(_) => "khonsu".to_string(),
        _ => command()
            .map(|cli| format!("\"{}\"", cli.display()))
            .unwrap_or_else(|_| "khonsu".to_string()),
    };
    format!(
        "KhonsuGraph is installed on this machine. Its command, {command}, queries local HawDB \
         graphs in Cypher or PostgreSQL SQL, syncs data sources into them, and checks .dash \
         dashboards; it prints JSON.\n\
         \n\
         - Start with `{command} --help` for the commands.\n\
         - To see what I have open in the KhonsuGraph window: `{command} open --state`.\n\
         - Read the labels, relationship types and properties before writing a query: \
         `{command} schema --db <graph>`.\n\
         - HawDB's Cypher has no sum(), labels() or path variables; \
         return whole nodes and relationships to get a graph back.\n\
         - Validate a dashboard with `{command} check <file.dash> --db <graph>`; \
         answer my comments on it with `{command} comments <file.dash>`.\n\
         - Reads are the default. Answer from the values it returns, check `truncated`, \
         and say which query produced them.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skill_installs_its_files_and_keeps_others() {
        let dir = std::env::temp_dir().join(format!("khonsu_skill_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "mine").unwrap();
        std::fs::write(dir.join("SKILL.md"), "an older skill").unwrap();

        install_skill_into(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("SKILL.md")).unwrap(), SKILL);
        assert!(dir.join("references/cli.md").exists());
        assert_eq!(std::fs::read_to_string(dir.join("notes.md")).unwrap(), "mine");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_prompt_names_commands_that_exist() {
        let prompt = agent_prompt();
        for command in ["--help", "open --state", "schema --db", "check ", "comments "] {
            assert!(prompt.contains(command), "{command}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn relinking_replaces_the_old_link() {
        let dir = std::env::temp_dir().join(format!("khonsu_link_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let target = std::env::current_exe().unwrap();
        let link = dir.join("bin/khonsu");
        link_directly(&target, &link).unwrap();
        assert!(same_file(&link, &target));
        link_directly(&target, &link).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
