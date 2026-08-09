use std::net::{SocketAddr, TcpListener};

use arc_swap::ArcSwap;
use axum::{
    Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use kithara_platform::{
    CancelToken,
    sync::Arc,
    tokio::{net::TcpListener as TokioListener, runtime::Builder as RuntimeBuilder},
};
use tower_http::cors::CorsLayer;

use crate::{BroadcastError, BroadcastResult, window::PlaylistSnapshot};

#[derive(Debug)]
pub(crate) struct Origin {
    pub(crate) snapshot: ArcSwap<PlaylistSnapshot>,
    pub(crate) master: Arc<str>,
}

pub(crate) fn start(
    bind: SocketAddr,
    origin: Arc<Origin>,
    token: CancelToken,
) -> BroadcastResult<SocketAddr> {
    let listener =
        TcpListener::bind(bind).map_err(|source| BroadcastError::Bind { addr: bind, source })?;
    let addr = listener
        .local_addr()
        .and_then(|addr| listener.set_nonblocking(true).map(|()| addr))
        .map_err(|source| BroadcastError::Bind { addr: bind, source })?;

    std::thread::Builder::new()
        .name(Consts::THREAD.to_owned())
        .spawn(move || serve(listener, origin, token))
        .map_err(|source| BroadcastError::Serve { addr, source })?;
    Ok(addr)
}

pub(crate) fn master_playlist(bit_rate: u64) -> String {
    let bandwidth = bit_rate + bit_rate / Consts::BANDWIDTH_MARGIN;
    format!(
        "#EXTM3U\n\
         #EXT-X-VERSION:3\n\
         #EXT-X-STREAM-INF:BANDWIDTH={bandwidth},CODECS=\"mp4a.40.2\"\n\
         {}\n",
        Consts::MEDIA_PLAYLIST
    )
}

struct Consts;

impl Consts {
    const BANDWIDTH_MARGIN: u64 = 20;

    const MEDIA_PLAYLIST: &'static str = "v/0/live.m3u8";

    const PLAYLIST_TYPE: &'static str = "application/vnd.apple.mpegurl";

    const SEGMENT_SUFFIX: &'static str = ".aac";

    const SEGMENT_TYPE: &'static str = "audio/aac";

    const THREAD: &'static str = "kithara-broadcast-origin";
}

fn serve(listener: TcpListener, origin: Arc<Origin>, token: CancelToken) {
    let runtime = match RuntimeBuilder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "the broadcast origin has no runtime");
            return;
        }
    };

    runtime.block_on(async move {
        let listener = match TokioListener::from_std(listener) {
            Ok(listener) => listener,
            Err(error) => {
                tracing::error!(%error, "the broadcast origin cannot take its socket");
                return;
            }
        };
        let router = Router::new()
            .route("/master.m3u8", get(master))
            .route("/v/0/live.m3u8", get(live))
            .route("/v/0/seg/{name}", get(segment))
            .layer(CorsLayer::permissive())
            .with_state(origin);

        let served = axum::serve(listener, router)
            .with_graceful_shutdown(async move { token.cancelled().await })
            .await;
        if let Err(error) = served {
            tracing::error!(%error, "the broadcast origin stopped serving");
        }
    });
}

async fn master(State(origin): State<Arc<Origin>>) -> Response {
    playlist(&origin.master)
}

async fn live(State(origin): State<Arc<Origin>>) -> Response {
    let snapshot = origin.snapshot.load();
    if snapshot.segments.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    playlist(&snapshot.playlist)
}

async fn segment(State(origin): State<Arc<Origin>>, Path(name): Path<String>) -> Response {
    let seq = name
        .strip_suffix(Consts::SEGMENT_SUFFIX)
        .and_then(|seq| seq.parse().ok());
    let bytes = seq.and_then(|seq| origin.snapshot.load().segment(seq));

    bytes.map_or_else(
        || StatusCode::NOT_FOUND.into_response(),
        |bytes| ([(header::CONTENT_TYPE, Consts::SEGMENT_TYPE)], bytes).into_response(),
    )
}

fn playlist(text: &str) -> Response {
    (
        [
            (header::CONTENT_TYPE, Consts::PLAYLIST_TYPE),
            (header::CACHE_CONTROL, "no-store"),
        ],
        text.to_owned(),
    )
        .into_response()
}
