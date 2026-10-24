#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Router, http::StatusCode, routing::get};
use futures::join;
use kithara_abr::Abr;
#[cfg(target_os = "android")]
use kithara_android as _;
use kithara_download::{Downloader, DownloaderConfig, DownloaderEvent, FetchCmd, Peer, RequestId};
use kithara_events::{EventBus, EventReceiver, EventSet};
use kithara_net::{HttpClient, NetOptions, RangeSpec, RetryPolicy};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Notify},
    time::{self, Duration, Instant},
    tokio::runtime::Handle,
};
use kithara_test_utils::{TestHttpServer, bufpool::pools, cancel_token, kithara};

#[derive(Clone, Debug, EventSet)]
enum Events {
    Downloader(DownloaderEvent),
}

struct RequestPeer(CancelToken);

impl Abr for RequestPeer {
    fn cancel(&self) -> CancelToken {
        self.0.clone()
    }
}

impl Peer for RequestPeer {}

fn request_id(event: &DownloaderEvent) -> RequestId {
    match event {
        DownloaderEvent::RequestEnqueued { request_id, .. }
        | DownloaderEvent::RequestStarted { request_id, .. }
        | DownloaderEvent::LoadSlow { request_id, .. }
        | DownloaderEvent::RequestCompleted { request_id, .. }
        | DownloaderEvent::RequestRetrying { request_id, .. }
        | DownloaderEvent::BodyStalled { request_id, .. }
        | DownloaderEvent::BodyResumed { request_id, .. }
        | DownloaderEvent::RetryExhausted { request_id, .. }
        | DownloaderEvent::FirstByte { request_id, .. }
        | DownloaderEvent::RequestFailed { request_id, .. }
        | DownloaderEvent::RequestCancelled { request_id, .. }
        | DownloaderEvent::PriorityChanged { request_id, .. } => *request_id,
    }
}

async fn drain(receiver: &mut EventReceiver<Events>) -> Vec<DownloaderEvent> {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut events = Vec::new();
    loop {
        let wait =
            Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now()));
        match time::timeout(wait, receiver.recv()).await {
            Ok(Ok(envelope)) => {
                let Events::Downloader(event) = envelope.event;
                events.push(event);
            }
            Ok(Err(error)) => panic!("event reception failed: {error:?}"),
            Err(_) => break,
        }
        assert!(Instant::now() < deadline, "event bus did not become idle");
    }
    events
}

#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(2)))]
#[case::get("get")]
#[case::range("range")]
#[case::head("head")]
async fn request_events_stay_with_their_request_and_peer(#[case] method: &str) {
    let attempts = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(Notify::default());
    let router = Router::new()
        .route(
            "/flaky",
            get({
                let attempts = Arc::clone(&attempts);
                let failed = Arc::clone(&failed);
                move || {
                    let attempts = Arc::clone(&attempts);
                    let failed = Arc::clone(&failed);
                    async move {
                        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            failed.notify_one();
                            (StatusCode::INTERNAL_SERVER_ERROR, "retry")
                        } else {
                            (StatusCode::OK, "ok")
                        }
                    }
                }
            }),
        )
        .route(
            "/ok",
            get(move || {
                let failed = Arc::clone(&failed);
                async move {
                    failed.notified().await;
                    "ok"
                }
            }),
        );
    let server = TestHttpServer::new(router).await;
    let flaky = server.url("/flaky");
    let ok = server.url("/ok");
    let options = NetOptions::builder()
        .inactivity_timeout(Duration::from_millis(500))
        .retry_policy(
            RetryPolicy::builder()
                .max_retries(2)
                .base_delay(Duration::from_millis(1))
                .max_delay(Duration::from_millis(5))
                .build(),
        )
        .build();
    let client = HttpClient::new(options, pools(), cancel_token());
    let downloader = Downloader::new(
        DownloaderConfig::for_client(client)
            .runtime(Handle::current())
            .build(),
    );
    let bus_a = EventBus::new(64);
    let bus_b = EventBus::new(64);
    let mut events_a = bus_a.subscribe::<Events>();
    let mut events_b = bus_b.subscribe::<Events>();
    let peer_a = downloader.register(Arc::new(RequestPeer(cancel_token())), Some(bus_a));
    let peer_b = downloader.register(Arc::new(RequestPeer(cancel_token())), Some(bus_b));
    let command = match method {
        "get" => FetchCmd::get(flaky.clone()).build(),
        "range" => FetchCmd::get(flaky.clone())
            .range(RangeSpec::new(0, Some(1)))
            .build(),
        "head" => FetchCmd::head(flaky.clone()).build(),
        _ => unreachable!(),
    };
    let (a, b) = join!(
        peer_a.execute(command),
        peer_b.execute(FetchCmd::get(ok.clone()).build())
    );
    let body_a = a
        .expect("peer A request")
        .body
        .collect()
        .await
        .expect("A body");
    let body_b = b
        .expect("peer B request")
        .body
        .collect()
        .await
        .expect("B body");
    assert_eq!(
        body_a.as_ref(),
        if method == "head" {
            &b""[..]
        } else {
            &b"ok"[..]
        }
    );
    assert_eq!(body_b.as_ref(), b"ok");
    let (events_a, events_b) = join!(drain(&mut events_a), drain(&mut events_b));
    let enqueued = |events: &[DownloaderEvent], url: &url::Url| {
        let ids: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                DownloaderEvent::RequestEnqueued {
                    request_id,
                    url: actual,
                    ..
                } => {
                    assert_eq!(actual, url);
                    Some(*request_id)
                }
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 1);
        ids[0]
    };
    let id_a = enqueued(&events_a, &flaky);
    let id_b = enqueued(&events_b, &ok);
    assert_ne!(id_a, id_b);
    assert!(events_a.iter().all(|event| request_id(event) == id_a));
    assert!(events_b.iter().all(|event| request_id(event) == id_b));
    assert!(
        events_a
            .iter()
            .any(|event| matches!(event, DownloaderEvent::RequestRetrying { attempt: 1, .. }))
    );
    if method != "head" {
        assert!(
            events_a
                .iter()
                .any(|event| matches!(event, DownloaderEvent::FirstByte { .. }))
        );
    }
    assert!(
        events_b
            .iter()
            .any(|event| matches!(event, DownloaderEvent::FirstByte { .. }))
    );
    assert!(
        !events_b
            .iter()
            .any(|event| matches!(event, DownloaderEvent::RequestRetrying { .. }))
    );
}
