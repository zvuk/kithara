//! # Signal generation route.
//! Provides procedural signal generation.
//!
//! ## Routes:
//! `GET /signal/sawtooth/{spec}.{ext}` — procedural signal generation.

use axum::{Router, extract::Path, http::StatusCode, response::IntoResponse, routing::get};

pub(crate) fn router() -> Router {
    Router::new().route("/signal/sawtooth/{spec}", get(sawtooth))
}

async fn sawtooth(Path(spec_with_ext): Path<String>) -> impl IntoResponse {
    let Some((_spec_b64, _ext)) = spec_with_ext.rsplit_once('.') else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    // TODO: decode base64url spec, generate signal artifact
    StatusCode::NOT_IMPLEMENTED.into_response()
}
