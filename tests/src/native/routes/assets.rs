use std::{path::PathBuf, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::Query,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use tower_http::services::ServeDir;

use crate::test_server_state::TestServerState;

pub(crate) fn router() -> Router<Arc<TestServerState>> {
    Router::new()
        .nest_service("/assets", ServeDir::new(assets_dir()))
        .route("/streamhq", get(streamhq))
}

pub(crate) fn assets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root from tests/")
        .join("assets")
}

/// Mirror the production `cdn-edge.zvq.me/track/streamhq?id=*` URL shape:
/// the path carries no file extension, so the only way a client can guess
/// the codec is via the `Content-Type` header on the response. Used by
/// `file_replay_from_warm_cache_mp3_no_extension` to pin that cold-cache
/// reload preserves the mime hint.
async fn streamhq(Query(params): Query<StreamHqQuery>) -> Response {
    let path = assets_dir().join(&params.name);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(err) => {
            return (
                StatusCode::NOT_FOUND,
                format!("asset `{}` not found: {err}", params.name),
            )
                .into_response();
        }
    };
    let content_type = mime_for(&params.name);
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, content_type.parse().expect("mime"));
    headers.insert(header::ACCEPT_RANGES, "bytes".parse().expect("static"));
    (headers, Body::from(bytes)).into_response()
}

#[derive(serde::Deserialize)]
struct StreamHqQuery {
    name: String,
}

fn mime_for(name: &str) -> &'static str {
    if name.ends_with(".mp3") {
        "audio/mpeg"
    } else if name.ends_with(".m4a") || name.ends_with(".aac") {
        "audio/mp4"
    } else if name.ends_with(".wav") {
        "audio/wav"
    } else {
        "application/octet-stream"
    }
}
