use rstest::*;
use std::path::PathBuf;
use tempfile::TempDir;

/// Common fixture for temporary directory
#[fixture]
pub fn temp_dir() -> TempDir {
    tempfile::tempdir().expect("Failed to create temp dir")
}

/// Fixture returning both TempDir and PathBuf
#[fixture]
pub fn temp_path() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("Failed to create temp dir");
    let path = dir.path().to_path_buf();
    (dir, path)
}
