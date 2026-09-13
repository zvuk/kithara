use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use kithara::platform::sync::Arc;

use crate::{
    test_server_state::TestServerState,
    token_store::{TokenRequest, TokenResponse},
};

pub(crate) fn router() -> Router<Arc<TestServerState>> {
    Router::new().route("/token", post(create_token))
}

async fn create_token(
    State(state): State<Arc<TestServerState>>,
    Json(request): Json<TokenRequest>,
) -> impl IntoResponse {
    match state.insert_hls_spec(request.hls_spec) {
        Ok(token) => Json(TokenResponse { token }).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}
