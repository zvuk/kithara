use kithara::{
    events::{EventReceiver, TrackId},
    platform::time::Duration,
    queue::QueueControl,
};
use kithara_test_fixtures::asset::Asset;

use super::OfflinePlayer;
use crate::{bufpool_ext::TestPools, event::TestEvent, waits::wait_for_loader_done_event};

/// How long a local fixture track may take to load through the queue loader.
pub const LOCAL_LOAD_DEADLINE: Duration = Duration::from_secs(20);

/// The queue source naming a fixture track on disk.
///
/// # Panics
///
/// Panics when the fixture has no file on disk.
#[must_use]
pub fn asset_source(track: &Asset) -> String {
    track
        .path()
        .expect("a queue fixture track lives on disk")
        .to_string_lossy()
        .into_owned()
}

/// Append a fixture track through the queue loader and wait until it loads.
pub async fn append_loaded(
    harness: &OfflinePlayer,
    queue: &QueueControl<TestPools>,
    track: &Asset,
) -> TrackId {
    append_source_loaded(harness, queue, asset_source(track)).await
}

/// Append `source` through the queue loader and wait until it loads.
///
/// # Panics
///
/// Panics when the append fails or the track does not load within
/// [`LOCAL_LOAD_DEADLINE`].
pub async fn append_source_loaded(
    harness: &OfflinePlayer,
    queue: &QueueControl<TestPools>,
    source: String,
) -> TrackId {
    let mut events: EventReceiver<TestEvent> = queue.subscribe();
    let id = harness
        .run(queue, move |q| q.append(source))
        .await
        .expect("append a local fixture through the queue loader");
    wait_for_loader_done_event(&mut events, queue, id, LOCAL_LOAD_DEADLINE)
        .await
        .unwrap_or_else(|err| panic!("local fixture {id:?} must load through the queue: {err}"));
    id
}
