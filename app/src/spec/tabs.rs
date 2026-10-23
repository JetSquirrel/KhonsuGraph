//! Dashboard spec files: telling one from other paths, and naming its tab.
//! The tabs open on each graph are remembered by the workspace.

use std::path::Path;


/// The extension a dashboard spec carries.
pub const EXTENSION: &str = "dash";

/// A path names a dashboard when it is a file with the spec's extension.
pub fn is_spec(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case(EXTENSION))
}

/// The default title of a dashboard tab: the file's stem.
pub fn title_for(path: &Path) -> String {
    path.file_stem()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A directory that removes itself, named for the test that made it.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "khonsu_specs_{label}_{}_{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::remove_dir_all(&path).ok();
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn spec(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, "query \"q\" { sql = \"SELECT 1\" }").unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn a_spec_is_a_dash_file_that_exists() {
        let directory = TempDir::new("is_spec");
        assert!(is_spec(&directory.spec("sales.dash")));
        assert!(is_spec(&directory.spec("upper.DASH")));
        assert!(!is_spec(&directory.spec("sales.csv")));
        assert!(!is_spec(&directory.0.join("gone.dash")));
        assert!(!is_spec(&directory.0));
    }

    #[test]
    fn a_title_is_the_file_stem() {
        assert_eq!(title_for(Path::new("/dash/sales.dash")), "sales");
        assert_eq!(title_for(Path::new("/")), "/");
    }
}
