//! An open whose origin answers late turns the track `Slow`, then `Loaded`,
//! and nothing beside the load gives up on it meanwhile.
#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex, PoisonError};

use kithara::{
    events::{EventReceiver, RecvError, TrackId},
    platform::time::{Duration, timeout},
    play::{ResourceConfig, ResourceSrc},
    queue::{QueueEvent, TrackSource, TrackStatus, Transition},
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper, event::TestEvent, kithara,
    offline::DiskQueue,
};
use kithara_test_fixtures::fixtures::tone_mp3;
use kithara_test_utils::temp_dir;

mod consts {
    use super::Duration;

    /// When the downloader calls the transfer slow.
    pub(super) const SOFT_TIMEOUT: Duration = Duration::from_millis(100);
    /// Past `SOFT_TIMEOUT` plus the one-second hang budget the test runs
    /// under, and far inside the net inactivity timeout.
    pub(super) const HEADERS_AFTER_MS: u64 = 2_000;
    pub(super) const LOAD_DEADLINE: Duration = Duration::from_secs(4);
}

/// Every panic message raised on any thread while the hook lives. Tasks
/// beside a load run detached, so their panics never reach the test thread.
fn record_panics() -> Arc<Mutex<Vec<String>>> {
    let panics = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&panics);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        recorded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(info.to_string());
        previous(info);
    }));
    panics
}

async fn statuses_until_loaded(
    events: &mut EventReceiver<TestEvent>,
    id: TrackId,
) -> Vec<TrackStatus> {
    let mut seen = Vec::new();
    loop {
        match events.recv().await {
            Ok(envelope) => {
                if let TestEvent::Queue(QueueEvent::TrackStatusChanged { id: track, status }) =
                    envelope.event
                    && track == id
                {
                    let done = matches!(status, TrackStatus::Loaded | TrackStatus::Failed(_));
                    seen.push(status);
                    if done {
                        return seen;
                    }
                }
            }
            Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => return seen,
        }
    }
}

#[kithara::test(tokio, native, timeout(Duration::from_secs(6)), hang_timeout_secs(1))]
async fn an_open_answered_late_turns_slow_then_loads(tone_mp3: &'static [u8]) {
    let panics = record_panics();
    let helper = TestServerHelper::new().await;
    let late = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(tone_mp3.to_vec()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::HeadersAfter {
            delay_ms: consts::HEADERS_AFTER_MS,
        },
    });
    let temp = temp_dir();
    let DiskQueue {
        queue,
        downloader,
        store,
        ticker: mut tick_handle,
        ..
    } = DiskQueue::builder(temp.path())
        .soft_timeout(consts::SOFT_TIMEOUT)
        .open()
        .await;
    let mut events = queue.subscribe();

    let url = late.child_url("track.mp3");
    let config = ResourceConfig::for_src(ResourceSrc::parse(url.as_str()).expect("valid URL"))
        .downloader(downloader)
        .store(store)
        .build();
    let id = queue
        .run(move |q| q.append(TrackSource::Config(Box::new(config))))
        .await
        .expect("append the late track");
    queue
        .run(move |q| q.select(id, Transition::None))
        .await
        .expect("select the late track");

    let seen = timeout(
        consts::LOAD_DEADLINE,
        statuses_until_loaded(&mut events, id),
    )
    .await
    .expect("the late track settles");
    assert_eq!(
        seen.last(),
        Some(&TrackStatus::Loaded),
        "statuses: {seen:?}"
    );
    assert!(
        seen.contains(&TrackStatus::Slow),
        "the late answer is reported slow first: {seen:?}"
    );
    let panics = panics
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert!(
        panics.is_empty(),
        "nothing beside a slow load may give up on it: {panics:?}"
    );

    tick_handle.stop().await;
    queue.close().await;
}
