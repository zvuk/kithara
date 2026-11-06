use std::path::PathBuf;

pub(crate) fn package() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo test package directory"))
}
