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
pub mod project;
pub mod report;
pub mod scope;
pub mod style;
pub mod suppress;
pub mod timestamp;
pub mod violation;
pub mod walker;
