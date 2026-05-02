//! Shared infrastructure for xtask static-analysis namespaces (`arch`, `style`, ...).
//!
//! Provides:
//! - `Violation` / `Severity` / `Report` — uniform check results
//! - `Baseline` / `RatchetDiff` — ratchet baseline plumbing
//! - `walker` — `.rs` discovery and glob matching
//! - `parse` — `syn` AST helpers (file parsing, scope/impl traversal, passthrough analysis)
//! - `report` — markdown / JSON renderers

pub(crate) mod baseline;
pub(crate) mod fix;
pub(crate) mod parse;
pub(crate) mod report;
pub(crate) mod scope;
pub(crate) mod suppress;
pub(crate) mod timestamp;
pub(crate) mod violation;
pub(crate) mod walker;
