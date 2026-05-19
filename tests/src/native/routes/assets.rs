use std::{path::PathBuf, sync::Arc};

use axum::Router;
use tower_http::services::ServeDir;

use crate::test_server_state::TestServerState;

pub(crate) fn router() -> Router<Arc<TestServerState>> {
    Router::new().nest_service("/assets", ServeDir::new(assets_dir()))
}

pub(crate) fn assets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root from tests/")
        .join("assets")
}
