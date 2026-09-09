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

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use kithara::{
    encode::EncoderFactory,
    events::{AdvanceReason, Event, QueueEvent, TrackId},
    platform::{
        sync::Arc,
        time::{self, Duration},
    },
    play::{Resource, ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, QueueControl, Transition, test_utils::QueueProbe},
    stream::AudioCodec,
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, HlsFixtureBuilder, TestServerHelper, TestTempDir,
    cochlea::CochleaReport,
    fixture_protocol::PcmPattern,
    offline::{OfflinePlayerHarness, OfflinePlayerOptions},
    temp_dir,
    test_defaults::packaged_content_frames,
};
use kithara_test_fixtures::{
    asset::Asset,
    assets,
    signal::{FrameClass, classify_windows},
};
use kithara_test_utils::probe::{IntoProbeArg, capture as probe_capture, capture::Recorder};

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
            Self::Hls => {
                let requested = usize::try_from(frames_from_secs(SEGMENT_SECS))
                    .expect("a segment carries a positive number of frames");
                let frame_samples = EncoderFactory::frame_samples(AudioCodec::Flac)
                    .expect("FLAC names its encoder frame size");
                let packaged = packaged_content_frames(requested, frame_samples, SEGMENTS)
                    .expect("the census fixture's packaged length fits usize");
                i64::try_from(packaged).expect("the packaged length fits the session axis")
            }
        }
    }

    /// Whether this origin's bytes arrive over HTTP.
    const fn needs_server(self) -> bool {
        match self {
            Self::Hls | Self::RemoteFlac | Self::RemoteMp3 => true,
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
        Origin::LocalFlac => ResourceSrc::Path(PathBuf::from(
            flac_asset(pattern)
                .path()
                .expect("a stored fixture names its store path"),
        )),
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

async fn open_resource(
    harness: &OfflinePlayerHarness,
    src: ResourceSrc,
    cache_dir: &Path,
) -> Resource {
    let config = ResourceConfig::<TestPools>::for_src(src)
        .store(kithara_integration_tests::disk_asset_store(cache_dir))
        .build();
    let config = harness
        .with_player(move |player| player.prepare_config(config))
        .await
        .expect("prepare census resource");
    let mut resource = Resource::new(config).await.expect("open census resource");
    let _ = resource.preload().await;
    resource
}

struct Census {
    harness: OfflinePlayerHarness,
    queue: QueueControl<TestPools>,
    tracks: Vec<TrackId>,
}

impl Census {
    async fn close(self) {
        let Self {
            harness,
            queue,
            tracks,
        } = self;
        drop(queue);
        drop(tracks);
        harness.close().await;
    }
}

async fn build_queue(sources: Vec<ResourceSrc>, temp_dir: &TestTempDir, seam: Seam) -> Census {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(seam.crossfade_seconds())
            .block_on_underrun(true)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    harness.set_host_level(CENSUS_LEVEL);
    let mut config = QueueConfig::builder().player(harness.take_player()).build();
    config.should_autoplay = false;
    let queue: QueueControl<TestPools> = harness.insert_control(Queue::new(config)).await;

    let mut tracks = Vec::with_capacity(sources.len());
    for (index, src) in sources.into_iter().enumerate() {
        let resource = open_resource(
            &harness,
            src,
            &temp_dir.path().join(format!("track{index}")),
        )
        .await;
        tracks.push(
            harness
                .run(&queue, move |q| q.insert_loaded_for_test(resource))
                .await,
        );
    }
    harness
        .run(&queue, {
            let arg0 = tracks[0];
            let arg1 = seam.transition();
            move |q| q.select(arg0, arg1)
        })
        .await
        .expect("select the first track");

    Census {
        harness,
        queue,
        tracks,
    }
}

/// Pace each block so the decode worker runs between them; without the yield
/// the worker never refills the ring and the queue starves mid-track.
fn render_block_duration() -> Duration {
    if cfg!(feature = "flash") {
        let frames = u32::try_from(BLOCK_FRAMES).expect("render block size fits u32");
        Duration::from_secs_f64(f64::from(frames) / f64::from(SAMPLE_RATE))
    } else {
        Duration::from_millis(1)
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
async fn play_to_the_end(census: &Census) -> (Vec<f32>, QueueLog) {
    let block_duration = render_block_duration();
    let mut receiver = census.queue.subscribe();
    let mut log = QueueLog::default();
    let mut rendered = Vec::new();

    for _ in 0..BLOCK_BUDGET {
        let _ = census.harness.run(&census.queue, |q| q.tick()).await;
        rendered.extend(census.harness.render(BLOCK_FRAMES).await);

        if let (Some(index), Some(duration)) = (
            census.queue.current_index(),
            census.queue.duration_seconds(),
        ) && duration > 0.0
        {
            log.durations.insert(index, duration);
        }

        while let Ok(envelope) = receiver.try_recv() {
            match envelope.event {
                Event::Queue(QueueEvent::CurrentTrackAdvance { id, reason }) => {
                    log.advances.push((id, reason));
                }
                Event::Queue(QueueEvent::CrossfadeStarted { .. }) => log.crossfades += 1,
                Event::Queue(QueueEvent::QueueEnded) => log.ended = true,
                _ => {}
            }
        }

        time::sleep(block_duration).await;

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

fn firings(recorder: &Recorder) -> Vec<Firing> {
    let mut firings: Vec<Firing> = recorder
        .events_with_probe("render")
        .iter()
        .filter_map(|event| {
            let base = event.u64("output_base")?;
            if base == u64::MAX {
                return None;
            }
            let range_start: i64 = i64::from_probe_arg(event.u64("range_start")?);
            Some(Firing {
                track: event.u64("track_id")?,
                block: i64::from_probe_arg(base) + range_start,
                served: event.u64("served_media_frames")?,
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
                window.served = after.served;
            })
            .or_insert(Active {
                first: before.block,
                last: after.block,
                served: after.served,
            });
    }
    windows
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
fn longest_silence(channel: &[f32]) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for sample in channel {
        if sample.abs() < SILENCE_THRESHOLD {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
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

async fn census_provenance(prepared: PreparedTracks, seam: Seam, temp_dir: &TestTempDir) -> Take {
    let recorder = probe_capture::install();
    let PreparedTracks {
        server: _server,
        origins,
        sources,
    } = prepared;
    let census = build_queue(sources, temp_dir, seam).await;
    let (rendered, log) = play_to_the_end(&census).await;

    assert!(
        log.ended,
        "the queue must reach its end within {BLOCK_BUDGET} blocks; \
         rendered {} frames",
        rendered.len() / usize::from(CHANNELS)
    );

    let windows = active_windows(&firings(&recorder));
    let expected: Vec<u64> = census.tracks.iter().map(|id| id.as_u64()).collect();
    let mut ordered: Vec<(u64, Active)> = windows.iter().map(|(id, w)| (*id, *w)).collect();
    ordered.sort_by_key(|(_, window)| window.first);

    assert_eq!(
        ordered.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        expected,
        "every queued track must produce audio, in queue order"
    );

    let block = i64::try_from(BLOCK_FRAMES).expect("block size fits the session axis");
    let slack = block * 4;
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

    let landed: Vec<u64> = log
        .advances
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
    assert!(
        log.advances.iter().all(|(_, reason)| matches!(
            reason,
            AdvanceReason::NaturalEof | AdvanceReason::CrossfadePreArm
        )),
        "a full playthrough may only advance at a track boundary — the \
         handover trigger and end-of-track race for it, so either reason is \
         the boundary's; got {:?}",
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

    census.close().await;
    Take { rendered, ordered }
}

/// The oracles that read the rendered audio rather than the probe: each
/// track's own ramp in queue order, no silence longer than the handover's
/// quantisation, and a seam that fades rather than sums. A lossy container
/// carries none of these, so a census over one runs the provenance half
/// alone.
///
/// `no_block`: seconds of frame classification over the whole take, after the
/// last await and over captured samples only. It occupies the poll it runs in
/// the way the budget is meant to catch, but there is no product work left to
/// starve.
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

    let silence = longest_silence(&left_channel(played));
    assert!(
        silence <= BLOCK_FRAMES,
        "the playthrough may fall silent only for as long as the handover is \
         quantised to: run={silence} frames, one render block is {BLOCK_FRAMES}"
    );

    let report = CochleaReport::measure(played, CHANNELS, SAMPLE_RATE);
    let track_peak_dbfs = 20.0 * f64::from(CENSUS_LEVEL).log10();
    let peak = report
        .sample_peak_dbfs
        .expect("a played take carries a sample peak");
    assert!(
        peak <= track_peak_dbfs + PEAK_BAND_DB,
        "a crossfade must fade the two tracks against each other rather than sum \
         them: peak={peak} dBFS, one track plays at {track_peak_dbfs} dBFS"
    );
    assert_eq!(
        report.leading_silence_ms, 0.0,
        "the queue must start on the first track's first frame: {report:?}"
    );
}

async fn run_census(prepared: PreparedTracks, seam: Seam, temp_dir: &TestTempDir) {
    let take = census_provenance(prepared, seam, temp_dir).await;
    census_acoustics(&take);
}

/// Gapless: no output frame may be claimed by two tracks at once. A premature
/// switch shows up here as an overlap the configuration never asked for.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(20)
)]
async fn gapless_hls_queue_plays_every_track_end_to_end(
    #[future(awt)] hls_tracks: PreparedTracks,
    temp_dir: TestTempDir,
) {
    run_census(hls_tracks, Seam::Gapless, &temp_dir).await;
}

/// Crossfade: the overlap must be exactly the configured one, at the boundary
/// and nowhere else.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(20)
)]
async fn crossfaded_hls_queue_plays_every_track_end_to_end(
    #[future(awt)] hls_tracks: PreparedTracks,
    temp_dir: TestTempDir,
) {
    run_census(hls_tracks, Seam::Crossfade, &temp_dir).await;
}

/// The same gapless census over local files: a track that arrives whole rather
/// than segment by segment must still be played to its last frame.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(20)
)]
async fn gapless_local_queue_plays_every_track_end_to_end(
    #[future(awt)] local_tracks: PreparedTracks,
    temp_dir: TestTempDir,
) {
    run_census(local_tracks, Seam::Gapless, &temp_dir).await;
}

/// The same crossfade census over local files.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(20)
)]
async fn crossfaded_local_queue_plays_every_track_end_to_end(
    #[future(awt)] local_tracks: PreparedTracks,
    temp_dir: TestTempDir,
) {
    run_census(local_tracks, Seam::Crossfade, &temp_dir).await;
}

/// The seam a playlist crosses when it leaves a segmented stream for a whole
/// body on a server, and crosses back. Every assertion the other legs carry
/// applies unchanged, because only the transport differs: the same ramp, the
/// same built length, the same provenance.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(20)
)]
async fn gapless_network_queue_plays_every_track_end_to_end(
    #[future(awt)] network_tracks: PreparedTracks,
    temp_dir: TestTempDir,
) {
    run_census(network_tracks, Seam::Gapless, &temp_dir).await;
}

/// The same seam with a crossfade: the overlap must be exactly the configured
/// one, which is the shape a track cut short breaks first.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(20)
)]
async fn crossfaded_network_queue_plays_every_track_end_to_end(
    #[future(awt)] network_tracks: PreparedTracks,
    temp_dir: TestTempDir,
) {
    run_census(network_tracks, Seam::Crossfade, &temp_dir).await;
}

/// A queue whose neighbours never share a reader: each seam hands over from a
/// segmented stream to a file, or back, and must still land on the frame.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(20)
)]
async fn gapless_mixed_queue_plays_every_track_end_to_end(
    #[future(awt)] mixed_tracks: PreparedTracks,
    temp_dir: TestTempDir,
) {
    run_census(mixed_tracks, Seam::Gapless, &temp_dir).await;
}

/// The same mixed queue with the crossfade: the overlap is the configured one
/// at a seam whose two sides are read differently.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(20)
)]
async fn crossfaded_mixed_queue_plays_every_track_end_to_end(
    #[future(awt)] mixed_tracks: PreparedTracks,
    temp_dir: TestTempDir,
) {
    run_census(mixed_tracks, Seam::Crossfade, &temp_dir).await;
}

/// The reported premature switch was seen where an HLS stream hands over to a
/// whole MPEG body on a server. The provenance half runs alone here: a lossy
/// container carries neither a readable ramp direction nor a peak the fade
/// oracle can read, so what is left is the attribution - each track serves the
/// length it was built to, and the seam overlaps by nothing at all.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(20)
)]
async fn gapless_mpeg_queue_serves_every_track_whole(
    #[future(awt)] mpeg_tracks: PreparedTracks,
    temp_dir: TestTempDir,
) {
    let _ = census_provenance(mpeg_tracks, Seam::Gapless, &temp_dir).await;
}

/// The same seam with the crossfade the reported defect was heard as: the
/// overlap must be exactly the configured one, and a track cut short shows up
/// as an overlap wider than the queue announced.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(20)
)]
async fn crossfaded_mpeg_queue_serves_every_track_whole(
    #[future(awt)] mpeg_tracks: PreparedTracks,
    temp_dir: TestTempDir,
) {
    let _ = census_provenance(mpeg_tracks, Seam::Crossfade, &temp_dir).await;
}

struct PreparedTracks {
    server: Option<TestServerHelper>,
    origins: [Origin; 3],
    sources: Vec<ResourceSrc>,
}

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
        sources.push(track_src(*origin, server.as_ref(), pattern).await);
    }
    PreparedTracks {
        server,
        origins,
        sources,
    }
}

#[kithara::fixture]
async fn hls_tracks() -> PreparedTracks {
    prepare_tracks(HLS_QUEUE).await
}

#[kithara::fixture]
async fn local_tracks() -> PreparedTracks {
    prepare_tracks(LOCAL_QUEUE).await
}

#[kithara::fixture]
async fn network_tracks() -> PreparedTracks {
    prepare_tracks(NETWORK_QUEUE).await
}

#[kithara::fixture]
async fn mixed_tracks() -> PreparedTracks {
    prepare_tracks(MIXED_QUEUE).await
}

#[kithara::fixture]
async fn mpeg_tracks() -> PreparedTracks {
    prepare_tracks(MPEG_QUEUE).await
}
