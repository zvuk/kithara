#![cfg(not(target_arch = "wasm32"))]

//! A three-track queue played from the first frame of the first track to the
//! last frame of the last, with every output frame attributed to the track
//! that produced it. The same census runs over both readers a track can arrive
//! through — segments streamed as HLS, and a whole FLAC file read from disk —
//! and over a queue that alternates between them at every seam.
//!
//! `PlayerTrack::render` names the track, the block-relative span it was asked
//! for, and the track's own media clock. What a track *actually* contributed
//! to a block is the clock's increase across it, not the span it was handed,
//! so the census reads a per-track active window on the session axis. Two
//! properties follow, and together they are the two halves of a premature
//! switch: a track must stay active for its whole length, and two tracks may
//! share output frames only inside a crossfade the queue announced.
//!
//! The rendered audio then answers the same question twice more without the
//! probe: its ramp direction says which track each stretch came from, and
//! Cochlea says the take never falls silent for longer than the handover's
//! block quantum and never sums two tracks above the level one plays at.

use std::collections::BTreeMap;

use ::kithara::{
    audio::AudioDecoderConfig,
    decode::DecoderBackend,
    events::TrackId,
    platform::{
        sync::{Arc, Mutex},
        time::{self, Duration},
        tokio::task,
    },
    play::{PlayerFactory, ResourceConfig, ResourceSrc, TrackFactory},
    queue::{AdvanceReason, Queue, QueueConfig, QueueControl, QueueEvent, TrackSource, Transition},
    stream::AudioCodec,
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, HlsFixtureBuilder, TestServerHelper,
    cochlea::CochleaReport,
    event::TestEvent,
    fixture_protocol::PcmPattern,
    hls_fixture::create_test_downloader,
    offline::{
        LOCAL_LOAD_DEADLINE, OfflinePlayer, OfflinePlayerOptions,
        linked::{LinkObservation, ObservedFactory},
    },
    smoothing,
    test_defaults::packaged_content_frames,
    usdt_trace::{self, ProbeEvent},
    waits::wait_for_loader_done_event,
};
use kithara_test_fixtures::{
    SignalAsset,
    asset::Asset,
    assets,
    hls_fixtures::frame_samples,
    signal::{FrameClass, classify_windows},
};
use kithara_test_utils::{TestTempDir, kithara, probe::IntoProbeArg, temp_dir};

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;
const SEGMENTS: usize = 3;
const SEGMENT_SECS: f64 = 2.0;
/// Length every fixture is built to, and the census's ruler.
///
/// Measuring a track against the duration the queue reports for it would let
/// the two agree while both are wrong: the reported duration is what arms the
/// crossfade, so a short report cuts the track *and* shortens the expectation.
/// The built length is the one number the playthrough cannot move.
const NOMINAL_TRACK_SECS: f64 = 6.0;
const CROSSFADE_SECS: f32 = 1.0;
/// Three tracks plus slack; the loop leaves early on `QueueEnded`.
const BLOCK_BUDGET: usize = 3_000;
/// Provenance classification window and its tolerance, as used by the
/// neighbouring boundary tests.
const CLASS_WINDOW: usize = 64;
const CLASS_TOL: f32 = 0.5;
/// Windows a class must hold to count as a track rather than a seam artefact:
/// a quarter second, against tracks that run for seconds.
const SUSTAINED_WINDOWS: usize = 172;
/// Level the offline session renders at, below the limiter's knee.
const CENSUS_LEVEL: f32 = 0.5;
/// Amplitude a sample counts as silence below, as the seam tests next door
/// read it. The fixture's ramp crosses zero under it for a few hundred
/// microseconds, which is two orders below a render block.
const SILENCE_THRESHOLD: f32 = 1.0e-3;
/// How far the take's peak may sit from the level one track plays at. A fade
/// law that attenuates keeps the sum under a single track's own peak; one that
/// does not doubles it, which is 6 dB away.
const PEAK_BAND_DB: f64 = 0.5;

/// Where one track's bytes come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    /// A three-segment media playlist served over HTTP.
    Hls,
    /// A whole FLAC file read from the fixture store by path.
    LocalFlac,
    /// The same whole FLAC body served over HTTP as one response.
    ///
    /// HLS resolves a manifest and asks for one segment at a time; a local
    /// file is opened by path and is there in full. Neither is the reader a
    /// playlist meets when it leaves a segmented stream for a file on a
    /// server, which asks for one body and reads it as it arrives.
    RemoteFlac,
    /// The same ramp as a whole MPEG body served over HTTP.
    ///
    /// MPEG reconciles its length with its audio least of all: the figure
    /// comes from a frame count in a header, and encoder delay and padding
    /// sit either side of it. That is the seam a playlist crosses when it
    /// leaves a segmented stream for a file on a music server.
    RemoteMp3,
    RemoteSignal(SignalAsset),
}

/// Every track streamed as HLS.
const HLS_QUEUE: [Origin; 3] = [Origin::Hls, Origin::Hls, Origin::Hls];
/// Every track read from a file.
const LOCAL_QUEUE: [Origin; 3] = [Origin::LocalFlac, Origin::LocalFlac, Origin::LocalFlac];
/// Readers alternate, so both seams hand over between two different ones.
const MIXED_QUEUE: [Origin; 3] = [Origin::Hls, Origin::LocalFlac, Origin::Hls];
/// The seam a playlist crosses when it leaves a segmented stream for a whole
/// body on a server, and crosses back.
const NETWORK_QUEUE: [Origin; 3] = [Origin::Hls, Origin::RemoteFlac, Origin::Hls];
/// The same seam, crossed into the container that declares its length least
/// exactly. This is the shape the reported premature switch was seen on.
const MPEG_QUEUE: [Origin; 3] = [Origin::Hls, Origin::RemoteMp3, Origin::Hls];

impl Origin {
    /// Frames of audio this origin's fixture actually carries.
    ///
    /// The stored file is written frame-exact. HLS packages the same ramp into
    /// segments of whole encoder frames, so it carries a little more than the
    /// nominal segment length asks for; the census has to measure against what
    /// was packaged rather than what was requested.
    fn built_frames(self) -> i64 {
        match self {
            Self::LocalFlac | Self::RemoteFlac | Self::RemoteMp3 => {
                frames_from_secs(NOMINAL_TRACK_SECS)
            }
            Self::RemoteSignal(asset) => {
                frames_from_secs(if asset.ext() == "ape" { 3.0 } else { 6.0 })
            }
            Self::Hls => {
                let requested = usize::try_from(frames_from_secs(SEGMENT_SECS))
                    .expect("a segment carries a positive number of frames");
                let frame_samples =
                    frame_samples(AudioCodec::Flac).expect("FLAC names its encoder frame size");
                let packaged = packaged_content_frames(requested, frame_samples, SEGMENTS)
                    .expect("the census fixture's packaged length fits usize");
                i64::try_from(packaged).expect("the packaged length fits the session axis")
            }
        }
    }

    /// Whether this origin's bytes arrive over HTTP.
    const fn needs_server(self) -> bool {
        match self {
            Self::Hls | Self::RemoteFlac | Self::RemoteMp3 | Self::RemoteSignal(_) => true,
            Self::LocalFlac => false,
        }
    }
}

/// What separates one track from the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seam {
    /// No overlap: the successor starts where the predecessor ended.
    Gapless,
    /// A `CROSSFADE_SECS` overlap the queue announces before it begins.
    Crossfade,
}

impl Seam {
    const fn crossfade_seconds(self) -> f32 {
        match self {
            Self::Gapless => 0.0,
            Self::Crossfade => CROSSFADE_SECS,
        }
    }

    const fn transition(self) -> Transition {
        match self {
            Self::Gapless => Transition::None,
            Self::Crossfade => Transition::Crossfade,
        }
    }
}

fn frames_from_secs(secs: f64) -> i64 {
    let frames = secs * f64::from(SAMPLE_RATE);
    num_traits::cast(frames).expect("a fixture duration fits the session axis")
}

/// The stored six-second body carrying this ramp.
fn flac_asset(pattern: PcmPattern) -> Asset {
    match pattern {
        PcmPattern::Ascending => assets::signal_flac_saw_6s(),
        PcmPattern::Descending => assets::signal_flac_saw_down_6s(),
        PcmPattern::ShiftedAscending => panic!("the census queues the two ramp directions only"),
    }
}

/// The same ramp stored as MPEG.
fn mp3_asset(pattern: PcmPattern) -> Asset {
    match pattern {
        PcmPattern::Ascending => assets::signal_mp3_saw_6s(),
        PcmPattern::Descending => assets::signal_mp3_saw_down_6s(),
        PcmPattern::ShiftedAscending => panic!("the census queues the two ramp directions only"),
    }
}

/// The source one track is read from. HLS packages the ramp into segments the
/// server hands out one request at a time; the local leg names a file the
/// reader can open whole; the remote leg serves that same file as one
/// range-capable body, which is what a file server gives a seeking reader, and
/// names it `.flac` because a bare body carries no other format hint. The
/// census does not distinguish them afterwards.
async fn track_src(
    origin: Origin,
    server: Option<&TestServerHelper>,
    pattern: PcmPattern,
) -> ResourceSrc {
    match origin {
        Origin::RemoteSignal(asset) => ResourceSrc::parse(
            server
                .expect("a signal fixture requires a server")
                .signal(asset)
                .as_str(),
        )
        .expect("valid signal URL"),
        Origin::Hls => {
            let created = server
                .expect("the HLS leg runs against a server")
                .create_hls(
                    HlsFixtureBuilder::new()
                        .variant_count(1)
                        .segments_per_variant(SEGMENTS)
                        .segment_duration_secs(SEGMENT_SECS)
                        .packaged_audio_per_variant_pcm_flac(SAMPLE_RATE, CHANNELS, vec![pattern]),
                )
                .await
                .expect("create census HLS fixture");
            ResourceSrc::parse(created.master_url().as_str()).expect("valid HLS master URL")
        }
        Origin::LocalFlac => ResourceSrc::Path(
            flac_asset(pattern)
                .path()
                .expect("a stored fixture names its store path"),
        ),
        Origin::RemoteFlac => {
            let handle = server
                .expect("the remote leg runs against a server")
                .register_behavior(FixtureBehavior {
                    content: Content::StaticBytes {
                        bytes: Arc::new(flac_asset(pattern).bytes().to_vec()),
                        content_type: Some("audio/flac"),
                    },
                    delivery: Delivery::Range,
                });
            ResourceSrc::parse(handle.child_url("track.flac").as_str())
                .expect("valid remote track URL")
        }
        Origin::RemoteMp3 => {
            let handle = server
                .expect("the MPEG leg runs against a server")
                .register_behavior(FixtureBehavior {
                    content: Content::StaticBytes {
                        bytes: Arc::new(mp3_asset(pattern).bytes().to_vec()),
                        content_type: Some("audio/mpeg"),
                    },
                    delivery: Delivery::Range,
                });
            ResourceSrc::parse(handle.child_url("track.mp3").as_str())
                .expect("valid remote track URL")
        }
    }
}

struct Census {
    harness: OfflinePlayer,
    queue: QueueControl<TestPools>,
    tracks: Vec<TrackId>,
    startup: Option<Arc<Mutex<LinkObservation>>>,
}

impl Census {
    async fn close(self) {
        let Self {
            harness,
            queue,
            tracks,
            startup,
        } = self;
        drop(queue);
        drop(tracks);
        drop(startup);
        harness.close().await;
    }
}

fn census_queue<F: TrackFactory<TestPools>>(
    harness: &OfflinePlayer,
    seam: Seam,
    factory: F,
) -> Queue<TestPools, F> {
    Queue::new(
        QueueConfig::with_factory(factory)
            .downloader(create_test_downloader())
            .prep(harness.resource_prep().clone())
            .settings(
                ::kithara::queue::QueueSettings::builder()
                    .crossfade(::kithara::play::CrossfadeSettings {
                        duration: seam.crossfade_seconds(),
                        ..::kithara::play::CrossfadeSettings::default()
                    })
                    .build(),
            )
            .build(),
    )
}

#[kithara::hang_watchdog(ctx = String)]
async fn build_queue(
    sources: Vec<ResourceSrc>,
    origins: [Origin; 3],
    seam: Seam,
    backend: Option<DecoderBackend>,
) -> Option<Census> {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(seam.crossfade_seconds())
            .block_on_underrun(true)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let explicit_store = backend.map(|_| {
        ::kithara::assets::AssetStore::builder(harness.resource_prep().worker.pools().clone())
            .build()
    });
    let startup = (origins == LOCAL_QUEUE && backend.is_none())
        .then(|| Arc::new(Mutex::new(LinkObservation::default())));
    let queue: QueueControl<TestPools> = if let Some(observation) = &startup {
        harness
            .insert_control(census_queue(
                &harness,
                seam,
                ObservedFactory::new(observation.clone()),
            ))
            .await
    } else {
        harness
            .insert_control(census_queue(&harness, seam, PlayerFactory))
            .await
    };
    harness
        .run(&queue, |control| control.set_level(CENSUS_LEVEL))
        .await
        .expect("set the mix level");

    let mut tracks = Vec::with_capacity(sources.len());
    let mut rejected = 0;
    for (source, origin) in sources.into_iter().zip(origins) {
        hang_tick!(format!("loading census track from {origin:?}"));
        let mut events = queue.subscribe();
        let source = match backend {
            Some(backend) => {
                let config = ResourceConfig::<TestPools>::for_src(source)
                    .downloader(create_test_downloader())
                    .store(
                        explicit_store
                            .as_ref()
                            .expect("explicit backend store")
                            .clone(),
                    )
                    .decoder(AudioDecoderConfig::builder().backend(backend).build())
                    .build();
                TrackSource::Config(Box::new(config))
            }
            None => TrackSource::Uri(source.to_string()),
        };
        let id = harness
            .run(&queue, move |control| control.append(source))
            .await
            .expect("append census track through the production loader");
        let loaded = wait_for_loader_done_event(&mut events, &queue, id, LOCAL_LOAD_DEADLINE).await;
        hang_reset!();
        let rejection = match origin {
            Origin::RemoteSignal(asset) => {
                kithara_integration_tests::fixtures::android_fixture_rejection(asset)
            }
            _ => None,
        };
        match (rejection, loaded) {
            (Some((expected, _)), Err(error)) => {
                assert_eq!(
                    error,
                    format!("track entered Failed: resource error: {expected}")
                );
                assert!(matches!(
                    queue
                        .track(id)
                        .expect("rejected track remains visible")
                        .status,
                    ::kithara::queue::TrackStatus::Failed(_)
                ));
                rejected += 1;
            }
            (Some((_, false)), Ok(())) => panic!("MediaCodec-only fixture unexpectedly loaded"),
            (_, Err(error)) => panic!("the census track must load: {error}"),
            (_, Ok(())) => {}
        }
        tracks.push(id);
    }
    if rejected > 0 {
        assert_eq!(
            rejected,
            tracks.len(),
            "the rejection census must reject every track"
        );
        assert!(
            queue.current().is_none(),
            "a rejected track must not become current"
        );
        assert!(
            harness
                .render(BLOCK_FRAMES)
                .await
                .iter()
                .all(|sample| *sample == 0.0)
        );
        drop(queue);
        harness.close().await;
        return None;
    }
    harness
        .run(&queue, {
            let first = tracks[0];
            let transition = seam.transition();
            move |control| control.select(first, transition)
        })
        .await
        .expect("select the first loaded track");

    Some(Census {
        harness,
        queue,
        tracks,
        startup,
    })
}

/// Hand the runtime back between blocks so the decode worker runs between
/// them; without the yield the worker never refills the ring and the queue
/// starves mid-track. On the virtual clock the pause is one block long, which
/// keeps time in step with the audio rendered. A real clock needs no pause:
/// `block_on_underrun` already holds each render until its audio is decoded,
/// and a sleep there only adds the timer's granularity to every block.
async fn pace_block() {
    if cfg!(feature = "flash") {
        let frames = u32::try_from(BLOCK_FRAMES).expect("render block size fits u32");
        time::sleep(Duration::from_secs_f64(
            f64::from(frames) / f64::from(SAMPLE_RATE),
        ))
        .await;
    } else {
        task::yield_runnable().await;
    }
}

#[derive(Default)]
struct QueueLog {
    advances: Vec<(Option<TrackId>, AdvanceReason)>,
    crossfades: usize,
    ended: bool,
    /// Reported duration of each queue position, read while it is current.
    durations: BTreeMap<usize, f64>,
}

#[kithara::flash(true)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
async fn play_to_the_end(census: &Census) -> (Vec<f32>, QueueLog) {
    let mut receiver = census.queue.subscribe();
    let mut log = QueueLog::default();
    let mut rendered = Vec::new();
    let mut observed = (None, None, 0, 0, false);

    for _ in 0..BLOCK_BUDGET {
        hang_tick!(format!(
            "queue census awaiting owner tick: index={:?}, position={:?}, advances={}, \
             crossfades={}, ended={}, playing={}, status={:?}, rendered={} frames",
            observed.0,
            observed.1,
            observed.2,
            observed.3,
            observed.4,
            census.queue.is_playing(),
            census.queue.status(),
            rendered.len() / 2,
        ));
        let _ = census.harness.run(&census.queue, QueueControl::tick).await;
        rendered.extend(census.harness.render(BLOCK_FRAMES).await);

        if let (Some(index), Some(duration)) = (
            census.queue.current_index(),
            census.queue.duration_seconds(),
        ) && duration > 0.0
        {
            log.durations.insert(index, duration);
        }

        while let Ok(envelope) = receiver.try_recv() {
            hang_tick!(format!("queue census drains events at {observed:?}"));
            match envelope.event {
                TestEvent::Queue(QueueEvent::CurrentTrackAdvance { id, reason }) => {
                    log.advances.push((id, reason));
                }
                TestEvent::Queue(QueueEvent::CrossfadeStarted { .. }) => log.crossfades += 1,
                TestEvent::Queue(QueueEvent::QueueEnded) => log.ended = true,
                _ => {}
            }
        }
        let progress = (
            census.queue.current_index(),
            census.queue.position_seconds(),
            log.advances.len(),
            log.crossfades,
            log.ended,
        );
        if progress != observed {
            observed = progress;
            hang_reset!();
        }

        pace_block().await;

        if log.ended {
            break;
        }
    }

    (rendered, log)
}

/// One firing of the render probe: which track was asked for which block, and
/// how much of that track had been served when it was asked.
#[derive(Clone, Copy, Debug)]
struct Firing {
    track: u64,
    block: i64,
    served: u64,
}

fn firings(records: &[ProbeEvent]) -> Vec<Firing> {
    let mut firings: Vec<Firing> = records
        .iter()
        .filter_map(|record| {
            let base = record.field("output_base")?;
            if base == u64::MAX {
                return None;
            }
            let range_start: i64 = i64::from_probe_arg(record.field("range_start")?);
            Some(Firing {
                track: record.field("track_id")?,
                block: i64::from_probe_arg(base) + range_start,
                served: record.field("served_media_frames")?,
            })
        })
        .collect();
    firings.sort_by_key(|firing| (firing.track, firing.block));
    firings
}

/// The session-axis span over which one track actually produced audio.
#[derive(Clone, Copy, Debug)]
struct Active {
    first: i64,
    last: i64,
    served: u64,
}

/// A track is active across a block when its media clock advanced over it, so
/// a block it was asked for but answered with EOF never enters the window.
fn active_windows(firings: &[Firing]) -> BTreeMap<u64, Active> {
    let mut windows: BTreeMap<u64, Active> = BTreeMap::new();
    for pair in firings.windows(2) {
        let (before, after) = (pair[0], pair[1]);
        if before.track != after.track || after.served <= before.served {
            continue;
        }
        windows
            .entry(before.track)
            .and_modify(|window| {
                window.last = after.block;
                window.served += after.served - before.served;
            })
            .or_insert(Active {
                first: before.block,
                last: after.block,
                served: after.served - before.served,
            });
    }
    windows
}

fn constant_clock_firings(raw_origin: u64, contributed: usize) -> Vec<Firing> {
    let mut offsets: Vec<usize> = (0..=contributed).step_by(BLOCK_FRAMES).collect();
    if offsets.last().copied() != Some(contributed) {
        offsets.push(contributed);
    }
    offsets
        .into_iter()
        .map(|offset| Firing {
            track: 7,
            block: 1_024 + i64::try_from(offset).expect("fixture output frame fits"),
            served: raw_origin + u64::try_from(offset).expect("fixture source frame fits"),
        })
        .collect()
}

#[kithara::test]
fn census_counts_a_healthy_track_from_its_nonzero_raw_origin() {
    let built = Origin::RemoteFlac.built_frames();
    let slack = i64::try_from(BLOCK_FRAMES).expect("fixture block fits") * 4;
    let contributed = usize::try_from(built).expect("fixture track length fits");
    for origin in [2_304, 4_420] {
        let windows = active_windows(&constant_clock_firings(origin, contributed));
        let window = windows.get(&7).expect("healthy track contributes");
        let served = i64::from_probe_arg(window.served);
        assert_eq!(served, built);
        assert_eq!(window.last - window.first, built);
        assert!((served - built).abs() <= slack);
    }
}

#[kithara::test]
fn census_does_not_count_requested_ranges_without_clock_progress() {
    let firings = [
        Firing {
            track: 7,
            block: 1_024,
            served: 4_420,
        },
        Firing {
            track: 7,
            block: 1_536,
            served: 4_420,
        },
        Firing {
            track: 7,
            block: 2_048,
            served: 4_932,
        },
        Firing {
            track: 7,
            block: 2_560,
            served: 4_932,
        },
    ];
    let windows = active_windows(&firings);
    let window = windows.get(&7).expect("one range contributes");
    assert_eq!(window.first, 1_536);
    assert_eq!(window.last, 2_048);
    assert_eq!(window.served, 512);
    assert!(active_windows(&firings[..2]).is_empty());
}

#[kithara::test]
fn census_rejects_a_shortened_contribution_hidden_by_its_raw_origin() {
    let built = Origin::RemoteFlac.built_frames();
    let slack = i64::try_from(BLOCK_FRAMES).expect("fixture block fits") * 4;
    let contributed = usize::try_from(built - 4_420).expect("shortened fixture length fits");
    let windows = active_windows(&constant_clock_firings(4_420, contributed));
    let window = windows.get(&7).expect("shortened track contributes");
    let served = i64::from_probe_arg(window.served);
    assert!(
        served < built - slack,
        "shortened contribution must remain rejected"
    );
    assert!(window.last - window.first < built - slack);
}

#[kithara::test]
fn census_rejects_excess_physical_contribution_with_a_nonzero_raw_origin() {
    let built = Origin::RemoteFlac.built_frames();
    let slack = i64::try_from(BLOCK_FRAMES).expect("fixture block fits") * 4;
    let contributed = usize::try_from(built + 4_420).expect("extended fixture length fits");
    let windows = active_windows(&constant_clock_firings(2_304, contributed));
    let window = windows.get(&7).expect("extended track contributes");
    let served = i64::from_probe_arg(window.served);
    assert!(
        served > built + slack,
        "excess contribution must remain rejected"
    );
    assert!(window.last - window.first > built + slack);
}

fn left_channel(rendered: &[f32]) -> Vec<f32> {
    rendered
        .chunks_exact(usize::from(CHANNELS))
        .map(|frame| frame[0])
        .collect()
}

/// Runs of one classification, ignoring the `Unknown` windows a crossfade's
/// mixed span produces.
///
/// The classifier reads a ramp's slope in the units the fixture was written
/// in, where one frame is one unit. The session plays at `CENSUS_LEVEL`, so the
/// take is undone by it first; classifying the attenuated ramp would leave
/// every window sitting on the tolerance's edge.
fn class_runs(rendered: &[f32]) -> Vec<(FrameClass, usize)> {
    let ramp: Vec<f32> = left_channel(rendered)
        .iter()
        .map(|sample| sample / CENSUS_LEVEL)
        .collect();
    let mut runs: Vec<(FrameClass, usize)> = Vec::new();
    for class in classify_windows(&ramp, CLASS_WINDOW, CLASS_TOL) {
        if matches!(class, FrameClass::Unknown) {
            continue;
        }
        match runs.last_mut() {
            Some((last, count)) if *last == class => *count += 1,
            _ => runs.push((class, 1)),
        }
    }
    runs
}

/// The take the census can speak for: everything up to the last block the last
/// track was active over. The render loop runs a couple of blocks past
/// `QueueEnded`, and that tail belongs to the loop, not to the queue.
fn played_samples(ordered: &[(u64, Active)]) -> usize {
    let last = ordered
        .last()
        .expect("the census names at least one track")
        .1
        .last;
    usize::try_from(last).expect("the session axis stays positive") * usize::from(CHANNELS)
}

/// Longest run of consecutive silent samples.
fn longest_silence(channel: &[f32]) -> std::ops::Range<usize> {
    let mut longest = 0..0;
    let mut run_start = 0;
    for (index, sample) in channel.iter().enumerate() {
        if sample.abs() < SILENCE_THRESHOLD {
            if index + 1 - run_start > longest.len() {
                longest = run_start..index + 1;
            }
        } else {
            run_start = index + 1;
        }
    }
    longest
}

/// The playthrough the provenance half attributed, handed to the oracles that
/// read the audio itself.
struct Take {
    rendered: Vec<f32>,
    ordered: Vec<(u64, Active)>,
}

fn trace_census(log: &QueueLog, firings: &[Firing], ordered: &[(u64, Active)], slack: i64) {
    tracing::info!(target: "kithara_tests::census", ?ordered, "queue census active windows");
    tracing::info!(target: "kithara_tests::census", durations = ?log.durations, "queue census durations");
    tracing::info!(target: "kithara_tests::census", advances = ?log.advances, "queue census advances");
    for firing in ordered.windows(2).flat_map(|pair| {
        let ((left_id, left), (right_id, right)) = (pair[0], pair[1]);
        firings
            .iter()
            .filter(move |firing| firing.track == left_id && firing.block >= left.last - slack)
            .take(8)
            .chain(
                firings
                    .iter()
                    .filter(move |firing| {
                        firing.track == right_id && firing.block <= right.first + slack
                    })
                    .take(8),
            )
    }) {
        tracing::info!(
            target: "kithara_tests::census",
            track = firing.track,
            output = firing.block,
            served = firing.served,
            "queue census seam firing"
        );
    }
}

fn trace_startup(census: &Census, firings: &[Firing], rendered: &[f32]) {
    let Some(observation) = &census.startup else {
        return;
    };
    let item = census.tracks[0].as_u64();
    let observed = observation.lock();
    tracing::info!(
        target: "kithara_tests::census",
        loads = ?observed.loads,
        starts = ?observed.starts,
        receipts = ?observed.placement_receipts,
        first_audible_frame = rendered
            .chunks_exact(usize::from(CHANNELS))
            .position(|frame| frame[0].abs() >= SILENCE_THRESHOLD),
        "queue census startup receipts"
    );
    for record in observed
        .timeline
        .iter()
        .filter(|record| record["item"].as_u64() == Some(item))
        .take(24)
    {
        tracing::info!(
            target: "kithara_tests::census",
            ?record,
            "queue census startup timeline"
        );
    }
    drop(observed);
    for firing in firings.iter().filter(|firing| firing.track == item).take(6) {
        tracing::info!(
            target: "kithara_tests::census",
            item,
            output = firing.block,
            served = firing.served,
            "queue census startup render"
        );
    }
}

async fn census_provenance(
    prepared: PreparedTracks,
    seam: Seam,
    _temp_dir: &TestTempDir,
) -> Option<Take> {
    let PreparedTracks {
        server: _server,
        origins,
        sources,
        backend,
    } = prepared;
    let census = build_queue(sources, origins, seam, backend).await?;
    let trace = usdt_trace::scope();
    let (rendered, log) = play_to_the_end(&census).await;
    let records = trace.events_of("render");

    assert!(
        log.ended,
        "the queue must reach its end within {BLOCK_BUDGET} blocks; \
         rendered {} frames",
        rendered.len() / usize::from(CHANNELS)
    );

    let firings = firings(&records);
    let windows = active_windows(&firings);
    let expected: Vec<u64> = census.tracks.iter().map(|id| id.as_u64()).collect();
    let mut ordered: Vec<(u64, Active)> = windows.iter().map(|(id, w)| (*id, *w)).collect();
    ordered.sort_by_key(|(_, window)| window.first);

    let block = i64::try_from(BLOCK_FRAMES).expect("block size fits the session axis");
    let slack = block * 4;
    trace_census(&log, &firings, &ordered, slack);

    assert_eq!(
        ordered.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        expected,
        "every queued track must produce audio, in queue order"
    );

    let lengths: Vec<i64> = origins
        .iter()
        .enumerate()
        .map(|(index, origin)| {
            let secs = *log
                .durations
                .get(&index)
                .unwrap_or_else(|| panic!("queue position {index} must report a duration"));
            let reported = frames_from_secs(secs);
            let built = origin.built_frames();
            assert!(
                (reported - built).abs() <= slack,
                "queue position {index} must report the length its fixture was \
                 built to: reported={reported} frames, built={built} +/- {slack}"
            );
            built
        })
        .collect();

    for ((id, window), track_frames) in ordered.iter().zip(&lengths) {
        let served = i64::from_probe_arg(window.served);
        assert!(
            (served - track_frames).abs() <= slack,
            "track {id} must serve its whole length: served={served} frames, \
             expected {track_frames} +/- {slack}"
        );
        let span = window.last - window.first;
        assert!(
            (span - track_frames).abs() <= slack,
            "track {id} must stay active for its whole length: span={span} frames, \
             expected {track_frames} +/- {slack}"
        );
    }

    let expected_overlap = frames_from_secs(f64::from(seam.crossfade_seconds()));
    for pair in ordered.windows(2) {
        let ((left_id, left), (right_id, right)) = (pair[0], pair[1]);
        let overlap = left.last - right.first;
        assert!(
            (overlap - expected_overlap).abs() <= slack,
            "tracks {left_id} and {right_id} must overlap by exactly the \
             configured crossfade: overlap={overlap} frames, expected \
             {expected_overlap} +/- {slack}"
        );
        assert!(
            right.first - left.last <= block,
            "no output frame may go unclaimed between tracks {left_id} and \
             {right_id}: the handover lands on the render-block grid, so one \
             block is the whole budget; gap={} frames",
            right.first - left.last
        );
    }

    let boundary_advances: Vec<_> = log
        .advances
        .iter()
        .filter(|(_, reason)| {
            matches!(
                reason,
                AdvanceReason::NaturalEof | AdvanceReason::CrossfadePreArm
            )
        })
        .collect();
    let landed: Vec<u64> = boundary_advances
        .iter()
        .filter_map(|(id, _)| id.map(TrackId::as_u64))
        .collect();
    assert_eq!(
        landed,
        expected[1..],
        "the queue must advance into each successor once, in order; \
         advances={:?}",
        log.advances
    );
    assert_eq!(
        log.crossfades,
        match seam {
            Seam::Gapless => 0,
            Seam::Crossfade => expected.len() - 1,
        },
        "a crossfade must be announced exactly at the configured boundaries"
    );

    trace_startup(&census, &firings, &rendered);
    census.close().await;
    drop(trace);
    Some(Take { rendered, ordered })
}

/// The oracles that read the rendered audio rather than the probe: each
/// track's own ramp in queue order, no silence longer than the handover's
/// quantisation, and a seam that fades rather than sums. A lossy container
/// carries none of these, so a census over one runs the provenance half
/// alone.
///
/// `no_block`: frame classification over the whole take and an analysis of
/// its opening second, after the last await and over captured samples only.
/// It occupies the poll it runs in the way the budget is meant to catch, but
/// there is no product work left to starve.
#[kithara::allow_block]
fn census_acoustics(take: &Take) {
    let rendered = take.rendered.as_slice();
    let ordered = take.ordered.as_slice();
    let runs = class_runs(rendered);
    let classes: Vec<FrameClass> = runs
        .iter()
        .filter(|(_, count)| *count >= SUSTAINED_WINDOWS)
        .map(|(class, _)| *class)
        .collect();
    assert_eq!(
        classes,
        vec![
            FrameClass::Ascending,
            FrameClass::Descending,
            FrameClass::Ascending
        ],
        "the rendered audio must carry each track's own signal, in queue order; \
         runs={runs:?}"
    );

    let played = &rendered[..played_samples(ordered)];

    let channel = left_channel(played);
    let worst = longest_silence(&channel);
    let leading = channel
        .iter()
        .take_while(|sample| sample.abs() < SILENCE_THRESHOLD)
        .count();
    let trailing = channel
        .iter()
        .rev()
        .take_while(|sample| sample.abs() < SILENCE_THRESHOLD)
        .count();
    let internal_end = channel.len().saturating_sub(trailing).max(leading);
    let internal = longest_silence(&channel[leading..internal_end]);
    tracing::info!(
        target: "kithara_tests::census",
        played_frames = channel.len(),
        worst_start = worst.start,
        worst_end = worst.end,
        leading_frames = leading,
        trailing_frames = trailing,
        internal_start = leading + internal.start,
        internal_end = leading + internal.end,
        internal_frames = internal.len(),
        ?ordered,
        "queue census acoustic silence locations"
    );
    for pair in ordered.windows(2) {
        let ((left_id, left), (right_id, right)) = (pair[0], pair[1]);
        let start = usize::try_from(left.last.min(right.first).max(0))
            .expect("positive seam frame")
            .saturating_sub(BLOCK_FRAMES);
        let end = usize::try_from(left.last.max(right.first).max(0))
            .expect("positive seam frame")
            .saturating_add(BLOCK_FRAMES)
            .min(channel.len());
        let run = longest_silence(&channel[start..end]);
        tracing::info!(
            target: "kithara_tests::census",
            left_id,
            right_id,
            left_last = left.last,
            right_first = right.first,
            silence_start = start + run.start,
            silence_end = start + run.end,
            silence_frames = run.len(),
            "queue census acoustic seam silence"
        );
    }
    let silence = worst.len();
    assert!(
        silence <= BLOCK_FRAMES,
        "the playthrough may fall silent only for as long as the handover is \
         quantised to: run={silence} frames, one render block is {BLOCK_FRAMES}"
    );

    let peak = 20.0 * f64::from(smoothing::peak(played)).log10();
    let track_peak_dbfs = 20.0 * f64::from(CENSUS_LEVEL).log10();
    assert!(
        peak <= track_peak_dbfs + PEAK_BAND_DB,
        "a crossfade must fade the two tracks against each other rather than sum \
         them: peak={peak} dBFS, one track plays at {track_peak_dbfs} dBFS"
    );

    // Leading silence is decided by the first analysis windows, which span
    // milliseconds: the opening second answers it exactly as the whole take
    // would, without the full analysis of everything after it.
    let opening_samples =
        usize::try_from(SAMPLE_RATE).expect("sample rate fits usize") * usize::from(CHANNELS);
    let report = CochleaReport::measure(
        &played[..played.len().min(opening_samples)],
        CHANNELS,
        SAMPLE_RATE,
    );
    assert_eq!(
        report.leading_silence_ms, 0.0,
        "the queue must start on the first track's first frame: {report:?}"
    );
}

async fn run_census(prepared: PreparedTracks, seam: Seam, temp_dir: &TestTempDir) {
    let take = census_provenance(prepared, seam, temp_dir)
        .await
        .expect("supported census produces a take");
    census_acoustics(&take);
}

/// Every track of a queue plays to its last frame across each seam. Gapless:
/// no output frame may be claimed by two tracks at once, so a premature switch
/// shows up as an overlap the configuration never asked for. Crossfade: the
/// overlap must be exactly the configured one, at the boundary and nowhere
/// else. The queues cover segmented streams, whole local files, whole bodies
/// on a server, and neighbours that never share a reader; only the transport
/// differs, so every assertion applies unchanged.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(20)
)]
#[case::gapless_hls(HLS_QUEUE, Seam::Gapless)]
#[case::crossfaded_hls(HLS_QUEUE, Seam::Crossfade)]
#[case::gapless_local(LOCAL_QUEUE, Seam::Gapless)]
#[case::crossfaded_local(LOCAL_QUEUE, Seam::Crossfade)]
#[case::gapless_network(NETWORK_QUEUE, Seam::Gapless)]
#[case::crossfaded_network(NETWORK_QUEUE, Seam::Crossfade)]
#[case::gapless_mixed(MIXED_QUEUE, Seam::Gapless)]
#[case::crossfaded_mixed(MIXED_QUEUE, Seam::Crossfade)]
async fn a_queue_plays_every_track_end_to_end(
    #[case] origins: [Origin; 3],
    #[case] seam: Seam,
    temp_dir: TestTempDir,
) {
    run_census(prepare_tracks(origins).await, seam, &temp_dir).await;
}

/// The reported premature switch was seen where an HLS stream hands over to a
/// whole MPEG body on a server. Only the provenance half runs: a lossy
/// container carries neither a readable ramp direction nor a peak the fade
/// oracle can read, so each track must serve the length it was built to, and
/// the seam must overlap by exactly what the queue announced.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(20)
)]
#[case::gapless(MPEG_QUEUE, Seam::Gapless)]
#[case::crossfaded(MPEG_QUEUE, Seam::Crossfade)]
#[case::profile_flac_flac_192000_2ch_16bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_16BIT); 3], Seam::Gapless)]
#[case::profile_flac_flac_192000_2ch_16bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_16BIT); 3], Seam::Crossfade)]
#[case::profile_flac_flac_192000_2ch_24bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_24BIT); 3], Seam::Gapless)]
#[case::profile_flac_flac_192000_2ch_24bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_192000_2CH_24BIT); 3], Seam::Crossfade)]
#[case::profile_flac_flac_22050_1ch_16bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_22050_1CH_16BIT); 3], Seam::Gapless)]
#[case::profile_flac_flac_22050_1ch_16bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_22050_1CH_16BIT); 3], Seam::Crossfade)]
#[case::profile_flac_flac_22050_2ch_16bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_22050_2CH_16BIT); 3], Seam::Gapless)]
#[case::profile_flac_flac_22050_2ch_16bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_22050_2CH_16BIT); 3], Seam::Crossfade)]
#[case::profile_flac_flac_44100_2ch_16bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_16BIT); 3], Seam::Gapless)]
#[case::profile_flac_flac_44100_2ch_16bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_16BIT); 3], Seam::Crossfade)]
#[case::profile_flac_flac_44100_2ch_24bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_24BIT); 3], Seam::Gapless)]
#[case::profile_flac_flac_44100_2ch_24bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_44100_2CH_24BIT); 3], Seam::Crossfade)]
#[case::profile_flac_flac_48000_2ch_16bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_16BIT); 3], Seam::Gapless)]
#[case::profile_flac_flac_48000_2ch_16bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_16BIT); 3], Seam::Crossfade)]
#[case::profile_flac_flac_48000_2ch_24bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_24BIT); 3], Seam::Gapless)]
#[case::profile_flac_flac_48000_2ch_24bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_48000_2CH_24BIT); 3], Seam::Crossfade)]
#[case::profile_flac_flac_88200_2ch_24bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_88200_2CH_24BIT); 3], Seam::Gapless)]
#[case::profile_flac_flac_88200_2ch_24bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_88200_2CH_24BIT); 3], Seam::Crossfade)]
#[case::profile_flac_flac_96000_2ch_16bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_16BIT); 3], Seam::Gapless)]
#[case::profile_flac_flac_96000_2ch_16bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_16BIT); 3], Seam::Crossfade)]
#[case::profile_flac_flac_96000_2ch_24bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_24BIT); 3], Seam::Gapless)]
#[case::profile_flac_flac_96000_2ch_24bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_FLAC_FLAC_96000_2CH_24BIT); 3], Seam::Crossfade)]
#[case::profile_mp3_libmp3lame_11025_1ch_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_MP3_LIBMP3LAME_11025_1CH); 3], Seam::Gapless)]
#[case::profile_mp3_libmp3lame_11025_1ch_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_MP3_LIBMP3LAME_11025_1CH); 3], Seam::Crossfade)]
#[case::profile_mp3_libmp3lame_22050_1ch_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_MP3_LIBMP3LAME_22050_1CH); 3], Seam::Gapless)]
#[case::profile_mp3_libmp3lame_22050_1ch_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_MP3_LIBMP3LAME_22050_1CH); 3], Seam::Crossfade)]
#[case::profile_mp3_libmp3lame_32000_2ch_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_MP3_LIBMP3LAME_32000_2CH); 3], Seam::Gapless)]
#[case::profile_mp3_libmp3lame_32000_2ch_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_MP3_LIBMP3LAME_32000_2CH); 3], Seam::Crossfade)]
#[case::profile_mp3_libmp3lame_44100_1ch_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_1CH); 3], Seam::Gapless)]
#[case::profile_mp3_libmp3lame_44100_1ch_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_1CH); 3], Seam::Crossfade)]
#[case::profile_mp3_libmp3lame_44100_2ch_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_2CH); 3], Seam::Gapless)]
#[case::profile_mp3_libmp3lame_44100_2ch_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_2CH); 3], Seam::Crossfade)]
#[case::profile_mp3_libmp3lame_48000_1ch_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_1CH); 3], Seam::Gapless)]
#[case::profile_mp3_libmp3lame_48000_1ch_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_1CH); 3], Seam::Crossfade)]
#[case::profile_mp3_libmp3lame_48000_2ch_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_2CH); 3], Seam::Gapless)]
#[case::profile_mp3_libmp3lame_48000_2ch_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_MP3_LIBMP3LAME_48000_2CH); 3], Seam::Crossfade)]
#[case::profile_m4a_aac_44100_2ch_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_M4A_AAC_44100_2CH); 3], Seam::Gapless)]
#[case::profile_m4a_aac_44100_2ch_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_M4A_AAC_44100_2CH); 3], Seam::Crossfade)]
#[case::profile_m4a_alac_44100_2ch_16bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_M4A_ALAC_44100_2CH_16BIT); 3], Seam::Gapless)]
#[case::profile_m4a_alac_44100_2ch_16bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_M4A_ALAC_44100_2CH_16BIT); 3], Seam::Crossfade)]
#[case::profile_ogg_vorbis_44100_2ch_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_OGG_VORBIS_44100_2CH); 3], Seam::Gapless)]
#[case::profile_ogg_vorbis_44100_2ch_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_OGG_VORBIS_44100_2CH); 3], Seam::Crossfade)]
#[case::profile_opus_libopus_48000_2ch_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_OPUS_LIBOPUS_48000_2CH); 3], Seam::Gapless)]
#[case::profile_opus_libopus_48000_2ch_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_OPUS_LIBOPUS_48000_2CH); 3], Seam::Crossfade)]
#[case::profile_aiff_pcm_s16be_44100_2ch_16bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_AIFF_PCM_S16BE_44100_2CH_16BIT); 3], Seam::Gapless)]
#[case::profile_aiff_pcm_s16be_44100_2ch_16bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_AIFF_PCM_S16BE_44100_2CH_16BIT); 3], Seam::Crossfade)]
#[case::profile_wav_pcm_f32le_192000_2ch_32bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_WAV_PCM_F32LE_192000_2CH_32BIT); 3], Seam::Gapless)]
#[case::profile_wav_pcm_f32le_192000_2ch_32bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_WAV_PCM_F32LE_192000_2CH_32BIT); 3], Seam::Crossfade)]
#[case::profile_wav_pcm_s16le_192000_2ch_16bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_WAV_PCM_S16LE_192000_2CH_16BIT); 3], Seam::Gapless)]
#[case::profile_wav_pcm_s16le_192000_2ch_16bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_WAV_PCM_S16LE_192000_2CH_16BIT); 3], Seam::Crossfade)]
#[case::profile_wav_pcm_s16le_44100_2ch_16bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_WAV_PCM_S16LE_44100_2CH_16BIT); 3], Seam::Gapless)]
#[case::profile_wav_pcm_s16le_44100_2ch_16bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_WAV_PCM_S16LE_44100_2CH_16BIT); 3], Seam::Crossfade)]
#[case::profile_wav_pcm_s24le_44100_2ch_24bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_WAV_PCM_S24LE_44100_2CH_24BIT); 3], Seam::Gapless)]
#[case::profile_wav_pcm_s24le_44100_2ch_24bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_WAV_PCM_S24LE_44100_2CH_24BIT); 3], Seam::Crossfade)]
#[case::profile_wav_pcm_s32le_192000_2ch_32bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_WAV_PCM_S32LE_192000_2CH_32BIT); 3], Seam::Gapless)]
#[case::profile_wav_pcm_s32le_192000_2ch_32bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_WAV_PCM_S32LE_192000_2CH_32BIT); 3], Seam::Crossfade)]
#[case::profile_ape_multiframe_44100_2ch_16bit_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_APE_MULTIFRAME_44100_2CH_16BIT); 3], Seam::Gapless)]
#[case::profile_ape_multiframe_44100_2ch_16bit_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_APE_MULTIFRAME_44100_2CH_16BIT); 3], Seam::Crossfade)]
#[case::profile_tagged_flac_id3_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_TAGGED_FLAC_ID3); 3], Seam::Gapless)]
#[case::profile_tagged_flac_id3_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_TAGGED_FLAC_ID3); 3], Seam::Crossfade)]
#[case::profile_tagged_mp3_id3_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_TAGGED_MP3_ID3); 3], Seam::Gapless)]
#[case::profile_tagged_mp3_id3_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_TAGGED_MP3_ID3); 3], Seam::Crossfade)]
#[case::profile_tagged_wave_mp3_id3_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_TAGGED_WAVE_MP3_ID3); 3], Seam::Gapless)]
#[case::profile_tagged_wave_mp3_id3_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_TAGGED_WAVE_MP3_ID3); 3], Seam::Crossfade)]
#[case::alac_silence_tail_gapless([Origin::RemoteSignal(SignalAsset::PROFILE_ALAC_SILENCE_TAIL); 3], Seam::Gapless)]
#[case::alac_silence_tail_crossfade([Origin::RemoteSignal(SignalAsset::PROFILE_ALAC_SILENCE_TAIL); 3], Seam::Crossfade)]
async fn an_encoded_queue_serves_every_track_whole(
    #[case] origins: [Origin; 3],
    #[case] seam: Seam,
    temp_dir: TestTempDir,
) {
    let _ = census_provenance(prepare_tracks(origins).await, seam, &temp_dir).await;
}

struct PreparedTracks {
    server: Option<TestServerHelper>,
    origins: [Origin; 3],
    sources: Vec<ResourceSrc>,
    backend: Option<DecoderBackend>,
}

#[kithara::hang_watchdog(ctx = String)]
async fn prepare_tracks(origins: [Origin; 3]) -> PreparedTracks {
    let server = if origins.iter().copied().any(Origin::needs_server) {
        Some(TestServerHelper::new().await)
    } else {
        None
    };
    let patterns = [
        PcmPattern::Ascending,
        PcmPattern::Descending,
        PcmPattern::Ascending,
    ];
    let mut sources = Vec::with_capacity(origins.len());
    for (origin, pattern) in origins.iter().zip(patterns) {
        hang_tick!(format!("preparing census source from {origin:?}"));
        {
            let _real_io = ::kithara::platform::flash::real_io();
            sources.push(track_src(*origin, server.as_ref(), pattern).await);
        }
        hang_reset!();
    }
    PreparedTracks {
        server,
        origins,
        sources,
        backend: None,
    }
}

#[cfg(not(target_os = "android"))]
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(20)
)]
#[case::profile_mp3_libmp3lame_44100_1ch_gapless(SignalAsset::PROFILE_MP3_LIBMP3LAME_44100_1CH)]
#[case::profile_mp3_libmp3lame_11025_1ch_gapless(SignalAsset::PROFILE_MP3_LIBMP3LAME_11025_1CH)]
async fn symphonia_mpeg_uses_the_same_full_playthrough_oracles(
    #[case] asset: SignalAsset,
    temp_dir: TestTempDir,
) {
    let mut prepared = prepare_tracks([Origin::RemoteSignal(asset); 3]).await;
    prepared.backend = Some(DecoderBackend::Symphonia);
    let _ = census_provenance(prepared, Seam::Gapless, &temp_dir).await;
}

#[cfg(not(target_os = "android"))]
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(20)
)]
#[case::gapless(Seam::Gapless)]
#[case::crossfade(Seam::Crossfade)]
async fn symphonia_aac_uses_the_same_full_playthrough_oracles(
    #[case] seam: Seam,
    temp_dir: TestTempDir,
) {
    let mut prepared =
        prepare_tracks([Origin::RemoteSignal(SignalAsset::PROFILE_M4A_AAC_44100_2CH); 3]).await;
    prepared.backend = Some(DecoderBackend::Symphonia);
    let _ = census_provenance(prepared, seam, &temp_dir).await;
}
