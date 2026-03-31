//! # Signal generation route.
//! Provides procedural signal generation.
//!
//! ## Routes:
//! - `GET /signal/sawtooth/{spec_with_ext}` — ascending saw-tooth.
//! - `GET /signal/sawtooth-desc/{spec_with_ext}` — descending saw-tooth.
//! - `GET /signal/sine/{spec_with_ext}` — sine wave.
//!
//! `spec_with_ext` is a single path segment in the form
//! `<base64url(JSON)>[.<ext>]`, where the optional last `.` separates the
//! encoded JSON payload from the target format extension.
//!
//! ## `spec_with_ext` format
//!
//! `spec_with_ext = <base64url(JSON)>[.<ext>]`
//!
//! ```json
//! {
//!   "ext": "wav",
//!   "sample_rate": 44100,
//!   "channels": 2,
//!   "seconds": 1.0,
//!   "freq": 440.0
//! }
//! ```
//!
//! - `ext`: `wav | mp3 | flac | aac | m4a`; can be passed in the path suffix or in JSON
//! - length: exactly one of `seconds`, `frames`, `file_bytes`, or `infinite`
//! - `freq` is required for `/signal/sine/...` and rejected for sawtooth routes

use axum::{
    Router,
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};

use crate::signal_spec::{SignalKind, parse_signal_request};

pub(crate) fn router() -> Router {
    Router::new()
        .route("/signal/sawtooth/{spec_with_ext}", get(sawtooth))
        .route(
            "/signal/sawtooth-desc/{spec_with_ext}",
            get(sawtooth_descending),
        )
        .route("/signal/sine/{spec_with_ext}", get(sine))
}

async fn sawtooth(Path(spec_with_ext): Path<String>) -> Response {
    handle_signal(SignalKind::Sawtooth, &spec_with_ext)
}

async fn sawtooth_descending(Path(spec_with_ext): Path<String>) -> Response {
    handle_signal(SignalKind::SawtoothDescending, &spec_with_ext)
}

async fn sine(Path(spec_with_ext): Path<String>) -> Response {
    handle_signal(SignalKind::Sine, &spec_with_ext)
}

fn handle_signal(kind: SignalKind, spec_with_ext: &str) -> Response {
    match parse_signal_request(kind, spec_with_ext) {
        Ok(_request) => {
            // TODO: use parsed request to build PCM and encode the requested artifact.
            StatusCode::NOT_IMPLEMENTED.into_response()
        }
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    use super::*;
    use crate::http_server::TestHttpServer;

    fn encode(json: &str) -> String {
        URL_SAFE_NO_PAD.encode(json)
    }

    #[tokio::test]
    async fn signal_routes_smoke_test() {
        let server = TestHttpServer::new(router()).await;
        let saw = encode(r#"{"seconds":1,"sample_rate":44100,"channels":2}"#);
        let sine = encode(r#"{"seconds":1,"sample_rate":44100,"channels":2,"freq":440}"#);
        let saw_json_ext = encode(r#"{"ext":"wav","seconds":1,"sample_rate":44100,"channels":2}"#);
        let valid_cases = [
            format!("/signal/sawtooth/{saw}.wav"),
            format!("/signal/sawtooth-desc/{saw}.wav"),
            format!("/signal/sine/{sine}.wav"),
            format!("/signal/sawtooth/{saw_json_ext}"),
        ];

        for path in valid_cases {
            let response = reqwest::get(server.url(&path)).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        }

        let invalid = reqwest::get(server.url("/signal/sine/not-base64.wav"))
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            invalid.text().await.unwrap(),
            "signal spec is not valid base64url"
        );
    }
}
