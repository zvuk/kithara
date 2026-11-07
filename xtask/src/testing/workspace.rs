use std::path::PathBuf;

pub(crate) fn root() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo test package directory"))
        .parent()
        .expect("xtask has a workspace root")
        .to_path_buf()
}
