//!
//! # Assets route.
//! Provides direct access to test assets.
//!
//! ## Routes:
//! `GET /assets/{path...}` — static test asset serving.

use std::path::PathBuf;

use axum::Router;
use tower_http::services::ServeDir;

pub(crate) fn router() -> Router {
    Router::new().nest_service("/assets", ServeDir::new(assets_dir()))
}

pub(crate) fn assets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("parent of kithara-test-utils")
        .parent()
        .expect("repo root")
        .join("assets")
}
