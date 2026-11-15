//! A marketplace folder: publishing into it, and installing from it by name.

mod common;

use khonsu_core::marketplace::{self, MarketError, Update};
use khonsu_core::plugin;
use std::fs;
use std::path::Path;

fn write_plugin(dir: &Path, version: &str) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("khonsu-plugin.json"),
        format!(r#"{{"id": "shelf", "name": "Shelf", "version": "{version}", "protocol": 1, "entry": ["cargo", "plugin.py"], "permissions": {{"read_files": true}}}}"#),
    )
    .unwrap();
    fs::write(dir.join("plugin.py"), "print()").unwrap();
    fs::write(dir.join("src").join("notes.txt"), "not part of the plugin").unwrap();
    fs::write(dir.join(".khonsuignore"), "# sources\nsrc/\n").unwrap();
}

#[test]
fn a_plugin_is_published_installed_by_name_and_updated() {
    let dir = common::TestDir::new();
    let root = dir.0.path();
    std::env::set_var("KHONSU_CONFIG_DIR", root.join("config"));
    marketplace::remove(marketplace::OFFICIAL).unwrap();
    assert!(marketplace::list().is_empty());

    let source = root.join("shelf");
    let market = root.join("market");
    write_plugin(&source, "0.1.0");
    let published = marketplace::publish(&source, &market, None, &[], None).unwrap();
    // Its entry is a program on PATH, so one package runs anywhere.
    assert_eq!(published.packed.target, "any");
    assert_eq!(published.packed.files, ["khonsu-plugin.json", "plugin.py"]);
    assert!(market.join("shelf-0.1.0-any.zip").is_file());

    let (added, index) = marketplace::add(market.to_str().unwrap(), None).unwrap();
    assert_eq!(added.name, "market");
    assert!(index.plugins[0].permissions.read_files);

    let (found, unreachable) = marketplace::available(Some("shel"), None).unwrap();
    assert!(unreachable.is_empty());
    assert_eq!((found[0].entry.id.as_str(), found[0].installed.clone(), found[0].target.as_deref()), ("shelf", None, Some("any")));
    assert!(marketplace::available(Some("nothing like it"), None).unwrap().0.is_empty());

    let installed = marketplace::install("shelf").unwrap();
    assert_eq!(installed.plugin.manifest.version, "0.1.0");
    // Installing is not approving.
    assert_eq!(plugin::approval_state(&installed.plugin), plugin::ApprovalState::NotApproved);
    assert!(matches!(marketplace::update(None, false).unwrap().as_slice(), [Update::Current { .. }]));

    // A new version replaces the old package in the folder and is offered.
    write_plugin(&source, "0.2.0");
    let published = marketplace::publish(&source, &market, None, &[], None).unwrap();
    assert_eq!(published.replaced.as_deref(), Some("0.1.0"));
    assert!(!market.join("shelf-0.1.0-any.zip").exists());
    assert!(marketplace::available(None, None).unwrap().0[0].update);
    plugin::approve(&installed.plugin).unwrap();
    let updates = marketplace::update(Some("shelf"), false).unwrap();
    let [Update::Updated { installed }] = updates.as_slice() else {
        panic!("{updates:?}");
    };
    assert_eq!(installed.replaced.as_deref(), Some("0.1.0"));
    // What was approved is not what is there now.
    assert_eq!(plugin::approval_state(&installed.plugin), plugin::ApprovalState::Changed);

    // Uninstalled, it is no longer something to update.
    plugin::uninstall("shelf").unwrap();
    assert!(marketplace::origins().is_empty());

    // A package that is not the one listed is not installed.
    let zip = market.join("shelf-0.2.0-any.zip");
    let mut bytes = fs::read(&zip).unwrap();
    bytes.extend_from_slice(b"tampered");
    fs::write(&zip, bytes).unwrap();
    assert!(matches!(marketplace::install("shelf@market"), Err(MarketError::Hash { .. })));
    assert!(plugin::find("shelf").is_err());
    assert!(matches!(marketplace::install("shelf@nowhere"), Err(MarketError::UnknownMarketplace(_))));
    assert!(matches!(marketplace::install("missing"), Err(MarketError::NoSuchPlugin(_))));
}
