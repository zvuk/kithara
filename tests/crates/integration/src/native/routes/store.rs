use std::path::Path;

use axum::{
    Router,
    body::Body,
    extract::Path as PathParam,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use kithara::platform::sync::Arc;
use kithara_test_fixtures::store;

use crate::test_server_state::TestServerState;

pub(crate) fn router() -> Router<Arc<TestServerState>> {
    Router::new().route("/store/{*path}", get(record))
}

async fn record(PathParam(path): PathParam<String>) -> Response {
    match store_record(&path) {
        Ok(bytes) => (
            [(header::CONTENT_TYPE, "application/octet-stream")],
            Body::from(bytes),
        )
            .into_response(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
            (StatusCode::BAD_REQUEST, error.to_string()).into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn store_record(path: &str) -> std::io::Result<Vec<u8>> {
    let path = store::file(Path::new(path))?;
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "fixture record is not a regular file",
        ));
    }
    std::fs::read(path)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use kithara::platform::sync::Arc;
    use kithara_test_fixtures::assets;

    use super::router;
    use crate::{http_server::TestHttpServer, kithara, test_server_state::TestServerState};

    #[kithara::test(tokio)]
    async fn store_route_serves_a_generated_record() {
        let asset = assets::sine_wav_a440_6s();
        let state = TestServerState::new();
        let server = TestHttpServer::new(router().with_state(Arc::clone(&state))).await;
        let response = reqwest::get(server.url(&format!("/store/{}", asset.entry().path)))
            .await
            .expect("GET store record");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.bytes().await.expect("record body").as_ref(),
            asset.bytes()
        );
    }

    #[kithara::test(tokio)]
    async fn store_route_answers_not_found_for_a_missing_record() {
        let state = TestServerState::new();
        let server = TestHttpServer::new(router().with_state(Arc::clone(&state))).await;
        let response = reqwest::get(server.url("/store/missing/no-such.wav"))
            .await
            .expect("GET missing store record");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[kithara::test(tokio)]
    async fn store_route_stays_reachable_while_the_network_is_offline() {
        let state = TestServerState::new();
        state.set_network_online(false);
        let server = TestHttpServer::new(crate::test_server::router(Arc::clone(&state))).await;
        let asset = assets::sine_wav_a440_6s();
        let response = reqwest::get(server.url(&format!("/store/{}", asset.entry().path)))
            .await
            .expect("GET store while offline");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.bytes().await.expect("record body").as_ref(),
            asset.bytes()
        );
    }
}
