//! Shared infrastructure for xtask static-analysis namespaces (`arch`, `style`, ...).
//!
//! Provides:
//! - `Violation` / `Severity` / `Report` — uniform check results
//! - `Baseline` / `RatchetDiff` — ratchet baseline plumbing
//! - `walker` — `.rs` discovery and glob matching
//! - `parse` — `syn` AST helpers (file parsing, scope/impl traversal, passthrough analysis)
//! - `report` — markdown / JSON renderers

pub mod baseline;
pub mod exclude;
pub mod fix;
pub mod parse;
pub(crate) mod process;
pub mod project;
pub mod report;
pub mod scope;
pub mod style;
pub mod suppress;
pub mod timestamp;
pub mod tools;
pub mod violation;
pub mod walker;

/// The libtest arguments that run one `#[ignore]`d test of this binary.
///
/// A test needing a real child spawns the test binary itself and names an
/// ignored test inside it. `module_path!()` expands at its call site, so it
/// arrives here as an argument rather than being read here.
#[cfg(test)]
pub(crate) fn child_test_args(module: &str, name: &str) -> Vec<String> {
    let module = module.split_once("::").map_or(module, |(_, module)| module);
    vec![
        format!("{module}::{name}"),
        "--exact".to_owned(),
        "--ignored".to_owned(),
        "--nocapture".to_owned(),
    ]
}
