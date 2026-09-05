//! Test-only helpers shared by the `ci` suites.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// Put the workspace's `fake-tool` where the code under test will look.
///
/// A unit test cannot ask Cargo where a binary target landed, so it is located
/// relative to this test binary: `target/<profile>/deps/<test>` sits one level
/// below the directory Cargo writes binaries into. `--bins` builds the binary
/// as a harness rather than as itself, which is why the message names the
/// build command.
pub(crate) fn install_double(bin: &Path, role: &str) -> PathBuf {
    let source = std::env::current_exe()
        .expect("current test executable")
        .parent()
        .and_then(Path::parent)
        .expect("the test binary lives under the profile directory")
        .join(format!("fake-tool{}", std::env::consts::EXE_SUFFIX));
    assert!(
        source.is_file(),
        "build the fake tool first: cargo build -p xtask --bin fake-tool ({})",
        source.display()
    );
    fs::create_dir_all(bin).expect("create the tool directory");
    let destination = bin.join(format!("{role}{}", std::env::consts::EXE_SUFFIX));
    fs::copy(&source, &destination).expect("install the fake tool");
    destination
}
