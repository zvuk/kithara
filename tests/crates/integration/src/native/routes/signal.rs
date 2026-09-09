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
    if !entry.path.ends_with(ext) {
        return (
            StatusCode::NOT_FOUND,
            format!("`{name}` is not stored as `.{ext}`"),
        )
            .into_response();
    }
    build_range_response(
        asset.bytes(),
        &headers,
        true,
        true,
        Some(entry.content_type),
    )
}
