use axum::{
    Router,
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use kithara::platform::sync::Arc;
use kithara_test_fixtures::assets::by_name;

use crate::{native::routes::range::build_range_response, test_server_state::TestServerState};

pub(crate) fn router() -> Router<Arc<TestServerState>> {
    Router::new().route("/signal/{name_with_ext}", get(signal))
}

/// Serves one generated body by the accessor name that produced it. The bytes
/// exist before the process starts: nothing here encodes.
async fn signal(Path(name_with_ext): Path<String>, headers: HeaderMap) -> Response {
    let Some((name, ext)) = name_with_ext.rsplit_once('.') else {
        return (
            StatusCode::BAD_REQUEST,
            format!("`{name_with_ext}` carries no file extension"),
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
    let entry = asset.entry();
    if entry.path.rsplit('.').next() != Some(ext) {
        return (
            StatusCode::NOT_FOUND,
            format!("`{name}` is not stored as `.{ext}`"),
        )
            .into_response();
    }
    tracing::debug!(
        target: "kithara_fixture_serving",
        event = "fixture_asset_bytes",
        phase = "begin",
        asset = entry.name,
    );
    let started = kithara::platform::time::WallInstant::now();
    let bytes = asset.bytes();
    let elapsed_ns = started.elapsed().as_nanos();
    tracing::debug!(
        target: "kithara_fixture_serving",
        event = "fixture_asset_bytes",
        phase = "end",
        asset = entry.name,
        bytes = bytes.len(),
        elapsed_ns = elapsed_ns,
    );
    build_range_response(bytes, &headers, true, true, Some(entry.content_type))
}
