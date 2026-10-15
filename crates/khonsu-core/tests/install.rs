//! Installing plugins from a `.zip`.

mod common;

use std::io::Write;
use std::path::Path;

use khonsu_core::plugin;

const MANIFEST: &str = r#"{"id": "zipped", "name": "Zipped", "version": "0.1.0", "protocol": 1, "entry": ["cargo", "plugin.py"]}"#;

fn write_zip(path: &Path, files: &[(&str, &str)]) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, text) in files {
        zip.start_file(*name, options).unwrap();
        zip.write_all(text.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn a_zip_installs_and_one_escaping_its_folder_does_not() {
    let dir = common::TestDir::new();
    std::env::set_var("KHONSU_CONFIG_DIR", dir.0.path().join("config"));

    // Zipped the usual way: the plugin folder itself inside the archive.
    let good = dir.0.path().join("zipped.zip");
    write_zip(&good, &[("zipped/khonsu-plugin.json", MANIFEST), ("zipped/plugin.py", "print()")]);
    let info = plugin::install(&good).unwrap();
    assert_eq!(info.id(), "zipped");
    assert!(info.dir.join("plugin.py").is_file());
    assert!(plugin::list().iter().any(|p| p.id() == "zipped"));

    let evil = dir.0.path().join("evil.zip");
    write_zip(&evil, &[("khonsu-plugin.json", MANIFEST), ("../escaped.txt", "x")]);
    let err = plugin::install(&evil).unwrap_err().to_string();
    assert!(err.contains("unsafe path"), "{err}");
    assert!(!dir.0.path().join("config").join("escaped.txt").exists());

    let bare = dir.0.path().join("bare.zip");
    write_zip(&bare, &[("readme.txt", "no manifest")]);
    let err = plugin::install(&bare).unwrap_err().to_string();
    assert!(err.contains("khonsu-plugin.json"), "{err}");
    // Nothing is left half-unpacked.
    let leftovers: Vec<_> = std::fs::read_dir(dir.0.path().join("config").join("plugins"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(leftovers, vec![std::ffi::OsString::from("zipped")]);
}
