#![allow(dead_code)]

use std::path::PathBuf;

/// A fresh directory for one test, removed on drop.
pub struct TestDir(pub tempfile::TempDir);

impl TestDir {
    pub fn new() -> Self {
        let base = std::env::var_os("KHONSU_TEST_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&base).unwrap();
        Self(tempfile::tempdir_in(base).unwrap())
    }

    pub fn graph(&self) -> PathBuf {
        self.0.path().join("graph")
    }
}
