#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{AssetStore, StorageBackend},
    events::{AudioEvent, Event, PlayerEvent},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{CancelToken, time::Duration},
    play::{PlayerConfig, PlayerImpl, ResourceConfig, ResourceSrc, SeekOutcome},
    queue::{PlaybackView, Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    CreatedHls, HlsFixtureBuilder, TestServerHelper, TestTempDir,
    bufpool_ext::{TestPools, pools},
    kithara,
    offline::OfflineQueue,
    temp_dir,
    waits::wait_for_loader_done_event,
};

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const SEGMENT_COUNT: usize = 3;
const SEGMENT_DURATION_SECS: f64 = 4.0;
const FINAL_SEGMENT: usize = SEGMENT_COUNT - 1;
const WARMUP_BLOCKS: usize = 2_048;
const SEEK_BLOCKS: usize = 2_048;
const MIN_WARMUP_SECS: f64 = 0.5;
const NEAR_END_OFFSET_SECS: f64 = 0.05;
const POSITION_TOLERANCE_SECS: f64 = 0.01;

#[derive(Clone, Copy, Debug)]
enum Target {
    NearEnd,
    End,
}

#[derive(Default)]
struct SeekEvents {
    end_of_stream: bool,
    item_ended: bool,
    seek_complete: bool,
    seek_rejected: bool,
}

fn render_and_tick(queue: &OfflineQueue<TestPools>) {
    let _ = queue.render(BLOCK_FRAMES);
    queue.tick().expect("tick queue");
}

fn drain_warmup(rx: &mut kithara::events::EventReceiver, latest_position: &mut Option<f64>) {
    while let Ok(envelope) = rx.try_recv() {
        if let Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. }) = envelope.event {
            *latest_position = Some(position_ms as f64 / 1000.0);
        }
    }
}

fn drain_seek_events(rx: &mut kithara::events::EventReceiver, observation: &mut SeekEvents) {
    while let Ok(envelope) = rx.try_recv() {
        match envelope.event {
            Event::Audio(AudioEvent::SeekComplete { .. }) => observation.seek_complete = true,
            Event::Audio(AudioEvent::SeekRejected { .. }) => observation.seek_rejected = true,
            Event::Audio(AudioEvent::EndOfStream { .. }) => observation.end_of_stream = true,
            Event::Player(PlayerEvent::ItemDidPlayToEnd { .. }) => observation.item_ended = true,
            _ => {}
        }
    }
}

fn known_duration(view: PlaybackView, phase: &str) -> f64 {
    view.duration
        .unwrap_or_else(|| panic!("duration became unknown {phase}"))
}

async fn run_case(
    helper: &TestServerHelper,
    fixture: CreatedHls,
    temp_dir: &TestTempDir,
    target_kind: Target,
) {
    let gate = helper.register_segment_gate(fixture.token(), 0, FINAL_SEGMENT);
    let pools = pools();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools.clone(),
            CancelToken::never(),
        ))
        .build(),
    );
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().into(),
        })
        .build();
    let sample_rate =
        std::num::NonZeroU32::new(SAMPLE_RATE).expect("fixture sample rate must be non-zero");
    let block_frames = std::num::NonZeroU32::new(
        u32::try_from(BLOCK_FRAMES).expect("fixture block size fits u32"),
    )
    .expect("fixture block size must be non-zero");
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(sample_rate)
            .worker(kithara::play::PlayWorker::new(
                kithara::play::PlayWorkerConfig::builder(pools.clone()).build(),
            ))
            .build(),
    );
    let queue = OfflineQueue::new(
        HostConfig::offline(pools)
            .sample_rate(sample_rate)
            .max_block_frames(block_frames)
            .build(),
        Queue::new(
            QueueConfig::builder()
                .player(player)
                .store(store.clone())
                .build(),
        ),
    )
    .await
    .expect("create product offline queue");
    let cfg = ResourceConfig::for_src(
        ResourceSrc::parse(fixture.master_url().as_str()).expect("valid HLS URL"),
    )
    .downloader(downloader)
    .store(store)
    .build();

    let mut rx = queue.subscribe();
    let id = queue
        .append(TrackSource::Config(Box::new(cfg)))
        .expect("append HLS track");
    queue
        .select(id, Transition::None)
        .expect("select HLS track");
    wait_for_loader_done_event(&mut rx, &queue, id, Duration::from_secs(30))
        .await
        .unwrap_or_else(|error| panic!("precondition: {error}"));

    let mut warmup_position = None;
    for _ in 0..WARMUP_BLOCKS {
        render_and_tick(&queue);
        drain_warmup(&mut rx, &mut warmup_position);
        if warmup_position.is_some_and(|position| position >= MIN_WARMUP_SECS)
            && queue.playback_view().duration.is_some()
            && gate.requested() > 0
        {
            break;
        }
    }
    let before = queue.playback_view();
    let duration = known_duration(before, "before the end seek");
    let warmup_position = warmup_position.unwrap_or(0.0);
    assert!(
        warmup_position >= MIN_WARMUP_SECS,
        "precondition: playback produced only {warmup_position:.3}s before the seek"
    );
    assert!(
        warmup_position < duration / 2.0,
        "precondition: playback had already reached {warmup_position:.3}s of \
         {duration:.3}s before the end seek"
    );
    assert!(
        gate.requested() > 0,
        "precondition: withheld final segment {FINAL_SEGMENT} was never requested; \
         the unbuffered-end window did not exist"
    );

    while rx.try_recv().is_ok() {}
    let target = match target_kind {
        Target::NearEnd => duration - NEAR_END_OFFSET_SECS,
        Target::End => duration,
    };
    let outcome = queue
        .seek(target)
        .unwrap_or_else(|error| panic!("seek to {target:.3}s failed: {error}"));
    match target_kind {
        Target::NearEnd => assert!(
            matches!(outcome, SeekOutcome::Landed { .. }),
            "near-end seek to {target:.3}s was classified as {outcome:?}"
        ),
        Target::End => assert!(
            matches!(
                outcome,
                SeekOutcome::Landed { .. } | SeekOutcome::PastEof { .. }
            ),
            "duration-boundary seek returned {outcome:?}"
        ),
    }

    gate.release();

    let mut observation = SeekEvents::default();
    for _ in 0..SEEK_BLOCKS {
        render_and_tick(&queue);
        drain_seek_events(&mut rx, &mut observation);
        let reached_outcome = match target_kind {
            Target::NearEnd => {
                observation.seek_complete || observation.seek_rejected || observation.item_ended
            }
            Target::End => {
                observation.item_ended
                    || observation.end_of_stream
                    || observation.seek_complete
                    || observation.seek_rejected
            }
        };
        if reached_outcome {
            break;
        }
    }
    assert!(
        !observation.seek_rejected,
        "seek to {target:.3}s was rejected after the final segment became available"
    );
    assert!(
        observation.seek_complete || observation.item_ended || observation.end_of_stream,
        "seek to {target:.3}s produced neither a committed seek nor a terminal event"
    );

    let after = queue.playback_view();
    let duration_after = known_duration(after, "after the end seek");
    assert!(
        (duration_after - duration).abs() < POSITION_TOLERANCE_SECS,
        "duration changed across a seek to {target:.3}s \
         ({duration:.3}s -> {duration_after:.3}s)"
    );
    let position_after = after.position.unwrap_or(0.0);
    assert!(
        position_after <= duration_after + POSITION_TOLERANCE_SECS,
        "position {position_after:.3}s exceeded duration {duration_after:.3}s \
         after seeking to {target:.3}s"
    );

    queue.clear();
    queue.close().await;
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn seek_to_duration_keeps_time_and_duration_consistent(
    temp_dir: TestTempDir,
    #[future(awt)] end_sources: (TestServerHelper, [CreatedHls; 2]),
) {
    let (helper, [near_end, end]) = end_sources;
    run_case(&helper, near_end, &temp_dir, Target::NearEnd).await;
    run_case(&helper, end, &temp_dir, Target::End).await;
}

#[kithara::fixture]
async fn end_sources() -> (TestServerHelper, [CreatedHls; 2]) {
    let helper = TestServerHelper::new().await;
    let first = end_source(&helper).await;
    let second = end_source(&helper).await;
    (helper, [first, second])
}

async fn end_source(helper: &TestServerHelper) -> CreatedHls {
    let fixture = helper
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(SEGMENT_COUNT)
                .segment_duration_secs(SEGMENT_DURATION_SECS)
                .packaged_audio_aac_lc(SAMPLE_RATE, 2),
        )
        .await
        .expect("create HLS fixture");
    fixture
}
