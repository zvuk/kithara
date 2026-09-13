use axum::{
    Router,
    body::Body,
    extract::{Path, Query},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use kithara::platform::sync::Arc;
use kithara_test_fixtures::{
    assets::by_name,
    hls::{HlsBundle, gapless_drm, gapless_plain, long_drm, long_plain, rss_plain},
};

use crate::test_server_state::TestServerState;

pub(crate) fn router() -> Router<Arc<TestServerState>> {
    Router::new()
        .route("/assets/hls/{*path}", get(plain_hls))
        .route("/assets/drm/{*path}", get(drm_hls))
        .route("/assets/hls-gapless/{*path}", get(plain_gapless_hls))
        .route("/assets/drm-gapless/{*path}", get(drm_gapless_hls))
        .route("/assets/hls-rss/{*path}", get(rss_hls))
        .route("/streamhq", get(streamhq))
}

async fn plain_hls(Path(path): Path<String>) -> Response {
    serve_hls(long_plain(), &path)
}

async fn drm_hls(Path(path): Path<String>) -> Response {
    serve_hls(long_drm(), &path)
}

async fn plain_gapless_hls(Path(path): Path<String>) -> Response {
    serve_hls(gapless_plain(), &path)
}

async fn drm_gapless_hls(Path(path): Path<String>) -> Response {
    serve_hls(gapless_drm(), &path)
}

async fn rss_hls(Path(path): Path<String>) -> Response {
    serve_hls(rss_plain(), &path)
}

fn serve_hls(bundle: &HlsBundle, path: &str) -> Response {
    let route = format!("/hls/{}", path.trim_start_matches('/'));
    let Some(resource) = bundle.get(&route) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match std::fs::read(resource.path()) {
        Ok(bytes) => (
            [(header::CONTENT_TYPE, resource.content_type())],
            Body::from(bytes),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(
                %route,
                path = %resource.path().display(),
                %error,
                "generated HLS resource is unreadable"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Mirror the production `cdn-edge.zvq.me/track/streamhq?id=*` URL shape:
/// the path carries no file extension, so the only way a client can guess
/// the codec is via the `Content-Type` header on the response. Used by
/// `file_replay_from_warm_cache_mp3_no_extension` to pin that cold-cache
/// reload preserves the mime hint.
///
/// `name` is `{accessor}.{ext}`, the same spelling the `/signal` route takes,
/// so both routes serve one generated body under one name.
async fn streamhq(Query(params): Query<StreamHqQuery>) -> Response {
    let Some((name, _ext)) = params.name.rsplit_once('.') else {
        return (
            StatusCode::BAD_REQUEST,
            format!("`{}` carries no file extension", params.name),
        )
            .into_response();
    };
    let Some(asset) = by_name(name) else {
        return (
            StatusCode::NOT_FOUND,
            format!("no generated asset is named `{name}`"),
        )
            .into_response();
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        asset.entry().content_type.parse().expect("mime"),
    );
    headers.insert(header::ACCEPT_RANGES, "bytes".parse().expect("static"));
    (headers, Body::from(asset.bytes())).into_response()
}

#[derive(serde::Deserialize)]
struct StreamHqQuery {
    name: String,
}
