//! The AWS plugin for the investigation and template gates. It lives in the
//! khonsugraph-marketplace repository with its saved responses and its
//! template, so these tests need a checkout of it: `KHONSU_MARKETPLACE`, or
//! `../khonsugraph-marketplace` beside this repository.
//!
//! The plugin is built there and installed into each test's own config
//! folder, approved, the way `khonsu plugin install aws --yes` would.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The marketplace checkout.
pub fn marketplace() -> PathBuf {
    let dir = std::env::var_os("KHONSU_MARKETPLACE").map(PathBuf::from).unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../khonsugraph-marketplace")
    });
    assert!(
        dir.join("plugins/aws/khonsu-plugin.json").is_file(),
        "these tests run the AWS plugin from a khonsugraph-marketplace checkout, and there is none at {}. \
         Clone https://github.com/JetSquirrel/khonsugraph-marketplace beside this repository, or set KHONSU_MARKETPLACE to one.",
        dir.display()
    );
    dir
}

/// The plugin's source folder in the marketplace.
pub fn source() -> PathBuf {
    marketplace().join("plugins/aws")
}

/// The saved responses of the `shop` account.
pub fn fixture() -> PathBuf {
    source().join("tests/fixture")
}

/// The saved responses with a positive, a negative and an undecided case, and
/// route tables. See the plugin's README.
pub fn exposure_fixture() -> PathBuf {
    source().join("tests/fixture-exposure")
}

/// The `aws.exposure` template the plugin ships.
pub fn exposure_template() -> PathBuf {
    source().join("templates/exposure.template.json")
}

/// Builds the plugin in the marketplace once, and lays it out as it is
/// packaged: the manifest, the executable and its templates.
pub fn package() -> &'static Path {
    static PACKAGE: OnceLock<PathBuf> = OnceLock::new();
    PACKAGE.get_or_init(|| {
        let marketplace = marketplace();
        let status = Command::new(env!("CARGO"))
            .args(["build", "-q", "--locked", "-p", "khonsu-plugin-aws"])
            .current_dir(&marketplace)
            .status()
            .unwrap();
        assert!(status.success(), "building the AWS plugin in {}", marketplace.display());
        let target = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| marketplace.join("target"));
        let exe = format!("khonsu-plugin-aws{}", std::env::consts::EXE_SUFFIX);

        let package = Path::new(env!("CARGO_TARGET_TMPDIR")).join("aws-plugin");
        let _ = std::fs::remove_dir_all(&package);
        std::fs::create_dir_all(package.join("templates")).unwrap();
        std::fs::copy(target.join("debug").join(&exe), package.join(&exe)).unwrap();
        std::fs::copy(source().join("khonsu-plugin.json"), package.join("khonsu-plugin.json")).unwrap();
        for template in std::fs::read_dir(source().join("templates")).unwrap() {
            let template = template.unwrap().path();
            std::fs::copy(&template, package.join("templates").join(template.file_name().unwrap())).unwrap();
        }
        package
    })
}

/// Installs and approves the plugin for the config folder `config`.
pub fn install(config: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_khonsugraph"))
        .args(["plugin", "install", package().to_str().unwrap(), "--yes"])
        .env("KHONSU_CONFIG_DIR", config)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "installing the AWS plugin: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
