//! `khonsu marketplace` and the `khonsu plugin` commands that go through one:
//! search, install by name, update, pack and publish. See
//! `khonsu_core::marketplace` and `docs/marketplace.md`.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use khonsu_core::marketplace::{self, MarketError, Update};
use khonsu_core::plugin;
use serde_json::{json, Value};

use crate::{CliError, NOT_ENFORCED};

#[derive(Subcommand)]
pub enum MarketplaceCommand {
    /// The marketplaces `khonsu plugin search` and `install` look in.
    List,
    /// Add a marketplace: an https address, owner/repo on GitHub, or a folder holding marketplace.json.
    Add {
        location: String,
        /// The name to know it by; otherwise the one its index gives.
        #[arg(long)]
        name: Option<String>,
    },
    /// Forget a marketplace. Plugins installed from it stay.
    Remove { name: String },
    /// Bring the official marketplace back after removing or replacing it.
    Restore,
}

#[derive(Args)]
pub struct PackArgs {
    /// The plugin folder, holding khonsu-plugin.json.
    dir: PathBuf,
    /// Where the archive goes.
    #[arg(long, short = 'o', default_value = ".")]
    out: PathBuf,
    /// The platform it runs on, such as macos-aarch64 or windows-x86_64, or
    /// `any`. Defaults to this machine's when the entry is a file in the
    /// package, and to `any` when it is a program found on PATH.
    #[arg(long)]
    target: Option<String>,
    /// A file to add at the top of the package: the executable a build left
    /// elsewhere, such as target/release/khonsu-plugin-x. Repeat for several.
    #[arg(long = "bin")]
    extra: Vec<PathBuf>,
}

#[derive(Args)]
pub struct PublishArgs {
    /// The plugin folder, holding khonsu-plugin.json.
    dir: PathBuf,
    /// The marketplace folder: its marketplace.json is updated, or made.
    #[arg(long)]
    to: PathBuf,
    /// As for `khonsu plugin pack`.
    #[arg(long)]
    target: Option<String>,
    /// As for `khonsu plugin pack`.
    #[arg(long = "bin")]
    extra: Vec<PathBuf>,
    /// List the package at <BASE_URL>/<file> instead of beside the index, for
    /// packages hosted somewhere else, such as a release's assets.
    #[arg(long)]
    base_url: Option<String>,
}

impl From<MarketError> for CliError {
    fn from(err: MarketError) -> Self {
        let kind = match &err {
            MarketError::Plugin(_) => "plugin",
            MarketError::Io(_) => "io",
            MarketError::UnknownMarketplace(_) | MarketError::NoSuchPlugin(_) => "not_found",
            MarketError::Hash { .. } => "hash_mismatch",
            MarketError::Fetch { .. } | MarketError::BadIndex { .. } => "marketplace",
            MarketError::Ambiguous { .. } | MarketError::Location(_) => "usage",
            MarketError::NoPackage { .. } => "no_package",
            MarketError::Pack(_) => "pack",
        };
        CliError::new(kind, err.to_string())
    }
}

pub fn run(command: MarketplaceCommand) -> Result<Value, CliError> {
    match command {
        MarketplaceCommand::List => Ok(json!({ "marketplaces": marketplace::list() })),
        MarketplaceCommand::Add { location, name } => {
            let (added, index) = marketplace::add(&location, name.as_deref())?;
            let plugins: Vec<Value> = index.plugins.iter().map(|e| json!({ "id": e.id, "version": e.version })).collect();
            Ok(json!({ "added": added, "plugins": plugins }))
        }
        MarketplaceCommand::Remove { name } => {
            marketplace::remove(&name)?;
            Ok(json!({ "removed": name }))
        }
        MarketplaceCommand::Restore => {
            marketplace::restore_official()?;
            Ok(json!({ "marketplaces": marketplace::list() }))
        }
    }
}

pub fn search(query: Option<&str>, only: Option<&str>) -> Result<Value, CliError> {
    let (plugins, unreachable) = marketplace::available(query, only)?;
    let mut out = json!({ "plugins": plugins });
    if !unreachable.is_empty() {
        out["unreachable"] = json!(unreachable);
    }
    Ok(out)
}

/// What to say after an install or update, approved or not.
fn approval_note(id: &str, approved: bool) -> String {
    if approved {
        NOT_ENFORCED.to_string()
    } else {
        format!("Run `khonsu plugin approve {id}` before syncing. {NOT_ENFORCED}")
    }
}

fn installed_json(installed: &marketplace::Installed, approved: bool) -> Value {
    let manifest = &installed.plugin.manifest;
    json!({
        "installed": manifest.id,
        "version": manifest.version,
        "replaced": installed.replaced,
        "marketplace": installed.marketplace,
        "target": installed.target,
        "sha256": installed.sha256,
        "dir": installed.plugin.dir,
        "permissions": manifest.permissions,
        "approved": approved,
        "sandboxed": false,
        "note": approval_note(&manifest.id, approved),
    })
}

pub fn install(spec: &str, yes: bool) -> Result<Value, CliError> {
    let looks_like_path = spec.contains(['/', '\\']) || spec.ends_with(".zip");
    if looks_like_path {
        return Err(CliError::usage(format!("{spec}: no such directory or archive")));
    }
    let installed = marketplace::install(spec)?;
    if yes {
        plugin::approve(&installed.plugin)?;
    }
    Ok(installed_json(&installed, yes))
}

pub fn update(only: Option<&str>, dry_run: bool, yes: bool) -> Result<Value, CliError> {
    let results = marketplace::update(only, dry_run)?;
    let mut out = Vec::new();
    for result in &results {
        out.push(match result {
            Update::Updated { installed } if dry_run => json!({
                "status": "would_update",
                "plugin": installed.plugin.manifest.id,
                "from": installed.replaced,
                "to": installed.plugin.manifest.version,
                "marketplace": installed.marketplace,
                "permissions": installed.plugin.manifest.permissions,
            }),
            Update::Updated { installed } => {
                if yes {
                    plugin::approve(&installed.plugin)?;
                }
                let mut value = installed_json(installed, yes);
                value["status"] = json!("updated");
                value
            }
            other => serde_json::to_value(other).expect("update serializes"),
        });
    }
    Ok(json!({ "dry_run": dry_run, "plugins": out }))
}

pub fn pack(args: PackArgs) -> Result<Value, CliError> {
    let packed = marketplace::pack(&args.dir, &args.out, args.target.as_deref(), &args.extra)?;
    Ok(serde_json::to_value(packed).expect("packed serializes"))
}

pub fn publish(args: PublishArgs) -> Result<Value, CliError> {
    let published = marketplace::publish(&args.dir, &args.to, args.target.as_deref(), &args.extra, args.base_url.as_deref())?;
    let mut out = serde_json::to_value(&published).expect("published serializes");
    out["next"] = json!(format!(
        "Share the folder {} (commit and push it, or upload it); others run `khonsu marketplace add <its address>` once, then `khonsu plugin install {}`.",
        args.to.display(),
        published.packed.id
    ));
    Ok(out)
}
