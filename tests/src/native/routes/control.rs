use axum::{
    Json, Router,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine as _, prelude::BASE64_STANDARD};
use kithara::platform::sync::Arc;
use kithara_test_fixtures::{assets::by_name, hls::long_plain};
use serde::{Deserialize, Serialize};

use crate::{
    test_server_state::{Content, Delivery, FixtureBehavior, TestServerState},
    token_store::TokenResponse,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NetworkRequest {
    pub online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ContentSpec {
    HtmlError,
    Status {
        code: u16,
    },
    Bytes {
        base64: String,
        content_type: Option<String>,
    },
    /// Serve one generated body, named the way `/signal/*` names it.
    ///
    /// An out-of-process client cannot upload a multi-megabyte body. Generated
    /// bodies are resolved by accessor name instead.
    Signal {
        name: String,
    },
    Asset {
        name: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum DeliverySpec {
    Normal,
    Range,
    EarlyClose { after_bytes: usize },
    StallAfter { after_bytes: usize },
    Throttle { chunk: usize, delay_ms: u64 },
    UnsizedEarlyClose { after_bytes: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BehaviorSpec {
    pub content: ContentSpec,
    pub delivery: DeliverySpec,
}

pub(crate) fn router() -> Router<Arc<TestServerState>> {
    Router::new()
        .route("/control/network", post(set_network))
        .route("/control/behavior", post(create_behavior))
}

async fn set_network(
    State(state): State<Arc<TestServerState>>,
    Json(request): Json<NetworkRequest>,
) -> impl IntoResponse {
    state.set_network_online(request.online);
    StatusCode::NO_CONTENT
}

fn content_from_spec(spec: ContentSpec) -> Result<Content, String> {
    match spec {
        ContentSpec::HtmlError => Ok(Content::HtmlError(
            "<html><body>503 Service Unavailable</body></html>",
        )),
        ContentSpec::Status { code } => Ok(Content::Status(code)),
        ContentSpec::Bytes {
            base64,
            content_type,
        } => {
            let bytes = BASE64_STANDARD
                .decode(base64)
                .map_err(|error| format!("invalid base64 body: {error}"))?;
            let content_type = match content_type.as_deref() {
                None => None,
                Some("application/vnd.apple.mpegurl") => Some("application/vnd.apple.mpegurl"),
                Some("audio/mpeg") => Some("audio/mpeg"),
                Some(other) => return Err(format!("unsupported content type: {other}")),
            };
            Ok(Content::StaticBytes {
                bytes: Arc::new(bytes),
                content_type,
            })
        }
        ContentSpec::Signal { name } => {
            let accessor = name
                .rsplit_once('.')
                .map_or(name.as_str(), |(stem, _)| stem);
            let asset =
                by_name(accessor).ok_or_else(|| format!("no generated asset is named `{name}`"))?;
            Ok(Content::StaticBytes {
                bytes: Arc::new(asset.bytes().to_vec()),
                content_type: Some(asset.entry().content_type),
            })
        }
        ContentSpec::Asset { name } => {
            let route = format!("/{}", name.trim_start_matches('/'));
            let resource = long_plain()
                .get(&route)
                .ok_or_else(|| format!("the generated HLS bundle has no `{name}`"))?;
            let bytes = std::fs::read(resource.path())
                .map_err(|error| format!("generated `{name}` is unreadable: {error}"))?;
            Ok(Content::StaticBytes {
                bytes: Arc::new(bytes),
                content_type: Some(resource.content_type()),
            })
        }
    }
}

const fn delivery_from_spec(spec: DeliverySpec) -> Delivery {
    match spec {
        DeliverySpec::Normal => Delivery::Normal,
        DeliverySpec::Range => Delivery::Range,
        DeliverySpec::EarlyClose { after_bytes } => Delivery::EarlyClose { after_bytes },
        DeliverySpec::StallAfter { after_bytes } => Delivery::StallAfter { after_bytes },
        DeliverySpec::Throttle { chunk, delay_ms } => Delivery::Throttle { chunk, delay_ms },
        DeliverySpec::UnsizedEarlyClose { after_bytes } => {
            Delivery::UnsizedEarlyClose { after_bytes }
        }
    }
}

#[cfg(test)]
fn register_behavior_spec(state: &TestServerState, spec: BehaviorSpec) -> String {
    let content = content_from_spec(spec.content).expect("valid content spec");
    state.insert_behavior(FixtureBehavior {
        content,
        delivery: delivery_from_spec(spec.delivery),
    })
}

async fn create_behavior(
    State(state): State<Arc<TestServerState>>,
    Json(spec): Json<BehaviorSpec>,
) -> Response {
    match content_from_spec(spec.content) {
        Ok(content) => {
            let token = state.insert_behavior(FixtureBehavior {
                content,
                delivery: delivery_from_spec(spec.delivery),
            });
            Json(TokenResponse { token }).into_response()
        }
        Err(error) => (StatusCode::BAD_REQUEST, error).into_response(),
    }
}

/// Reject every data route while the global network switch is offline.
///
/// `/control/*` remains reachable so callers can restore the network, and
/// `/health` remains available for process-level liveness checks.
pub(crate) async fn network_guard(
    State(state): State<Arc<TestServerState>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let exempt = path.starts_with("/control/") || path == "/health";
    if !exempt && !state.network_online() {
        return (StatusCode::SERVICE_UNAVAILABLE, "network offline").into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{kithara, test_server_state::TestServerState};

    #[kithara::test]
    fn network_starts_online() {
        let state = TestServerState::new();
        assert!(state.network_online(), "server must start reachable");
    }

    #[kithara::test]
    fn network_switch_flips_both_ways() {
        let state = TestServerState::new();
        state.set_network_online(false);
        assert!(
            !state.network_online(),
            "switch must take the server offline"
        );
        state.set_network_online(true);
        assert!(state.network_online(), "switch must bring the server back");
    }

    #[kithara::test]
    fn signal_spec_serves_a_generated_body() {
        let content = content_from_spec(ContentSpec::Signal {
            name: "signal_mp3_track_sine440_187s.mp3".to_owned(),
        })
        .expect("the generated MP3 must resolve");
        let Content::StaticBytes {
            bytes,
            content_type,
        } = content
        else {
            panic!("signal spec must resolve to static bytes");
        };
        assert_eq!(
            bytes.as_slice(),
            kithara_test_fixtures::assets::signal_mp3_track_sine440_187s().bytes(),
        );
        assert_eq!(content_type, Some("audio/mpeg"));
    }

    #[kithara::test]
    fn signal_spec_rejects_an_unregistered_name() {
        let Err(error) = content_from_spec(ContentSpec::Signal {
            name: "signal_mp3_not_a_generator.mp3".to_owned(),
        }) else {
            panic!("an unregistered name must be rejected");
        };
        assert!(
            error.contains("no generated asset is named"),
            "unexpected rejection reason: {error}"
        );
    }

    #[kithara::test]
    fn asset_spec_serves_a_file_of_the_generated_hls_bundle() {
        let content = content_from_spec(ContentSpec::Asset {
            name: "hls/index-slq-a1.m3u8".to_owned(),
        })
        .expect("the generated variant playlist must resolve");
        let Content::StaticBytes {
            bytes,
            content_type,
        } = content
        else {
            panic!("asset spec must resolve to static bytes");
        };
        assert!(bytes.starts_with(b"#EXTM3U"));
        assert_eq!(content_type, Some("application/vnd.apple.mpegurl"));
    }

    #[kithara::test]
    fn behavior_registration_yields_a_token() {
        let state = TestServerState::new();
        let token = register_behavior_spec(
            &state,
            BehaviorSpec {
                content: ContentSpec::Status { code: 503 },
                delivery: DeliverySpec::Normal,
            },
        );
        assert!(
            state.get_behavior(&token).is_some(),
            "registered behavior must be retrievable by its token"
        );
    }
}
