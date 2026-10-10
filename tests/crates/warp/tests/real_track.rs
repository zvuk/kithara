//! Warp on downloaded library tracks: every beat the fixture build analysed
//! lands on the Host beat the plan projects it to, at steady, ramped and
//! retargeted Host tempos.

use std::{io::Cursor, num::NonZero};

use kithara::{
    decode::{DecoderChunkOutcome, DecoderConfig, DecoderFactory},
    platform::time::Duration,
    resampler::NoResamplerBackend,
    signal::{AudioSpec, FrameCount, InterleavedView, SessionEpoch, SessionFrame},
    stretch::StretchKind,
    warp::{
        Beat, BeatAlignment, BeatGridId, BeatGridRevision, BeatGridSnapshot, MapPoint,
        SessionAnchor, SessionBeat, WarpConfig, WarpMap, WarpMapRevision,
    },
};
use kithara_integration_tests::{
    audio_artifact::AudioArtifactTap,
    grid::{Start, analysed_grid},
};
use kithara_test_fixtures::assets::by_name;
use num_traits::ToPrimitive;

use crate::{
    region::{CH, Presented, Projection, Timeline, render_configured_grid_with_updates},
    test_pools::pools,
};

mod consts {
    /// A presented chunk may publish a source frame this many output frames
    /// off the Host frame its analysed beat projects to: the renderer rounds
    /// each endpoint to a whole frame.
    pub(super) const TOLERANCE_FRAMES: f64 = 2.0;
    /// Source beats fed past the last Host beat: a retarget's transition
    /// consumes source the steady plan would not.
    pub(super) const SOURCE_MARGIN_BEATS: usize = 8;
    pub(super) const SECONDS_PER_MINUTE: f64 = 60.0;
}

/// One Host tempo trajectory: its initial tempo, then `(seconds, bpm)`
/// retargets approached over `smooth_seconds`, rendered for `seconds`.
struct Trajectory {
    name: &'static str,
    start_bpm: f64,
    retargets: &'static [(usize, f64)],
    smooth_seconds: f64,
    seconds: usize,
}

const TRAJECTORIES: &[Trajectory] = &[
    Trajectory {
        name: "fixed-96",
        start_bpm: 96.0,
        retargets: &[],
        smooth_seconds: 0.0,
        seconds: 10,
    },
    Trajectory {
        name: "fixed-160",
        start_bpm: 160.0,
        retargets: &[],
        smooth_seconds: 0.0,
        seconds: 10,
    },
    Trajectory {
        name: "rise-96-to-150",
        start_bpm: 96.0,
        retargets: &[(0, 150.0)],
        smooth_seconds: 3.0,
        seconds: 12,
    },
    Trajectory {
        name: "ride-96-150-110-160-100",
        start_bpm: 96.0,
        retargets: &[(0, 150.0), (8, 110.0), (16, 160.0), (24, 100.0)],
        smooth_seconds: 2.5,
        seconds: 32,
    },
];

/// A downloaded library track: its fixture name and decode hint.
#[derive(Clone, Copy, Debug)]
struct Library {
    name: &'static str,
    extension: &'static str,
}

/// A library track decoded at its native shape from its start beat through
/// the source the trajectories read, with the build-time analysed
/// beats as `(ordinal, source frame)` of that slice.
struct Track {
    name: &'static str,
    start: Start,
    spec: AudioSpec,
    pcm: Vec<f32>,
    beats: Vec<(f64, f64)>,
    model: kithara::beat::BeatGridModel,
    bpm: f64,
}

impl Track {
    fn load(library: Library, start: Start, trajectories: &[Trajectory]) -> Self {
        let Library { name, extension } = library;
        let analysed = analysed_grid(name);
        let raw = analysed.as_raw();
        let first = start.beat(&analysed);
        let config = DecoderConfig::<NoResamplerBackend, crate::test_pools::TestPools>::builder()
            .pools(pools())
            .build();
        let asset = by_name(name).unwrap_or_else(|| panic!("`{name}` is not registered"));
        let mut decoder = DecoderFactory::create_with_probe(
            Cursor::new(asset.bytes().to_vec()),
            Some(extension),
            config,
        )
        .unwrap_or_else(|error| panic!("open `{name}`: {error:?}"));
        let spec = decoder.spec();
        assert_eq!(usize::from(spec.channels), CH, "`{name}` is stereo");
        let rate = f64::from(spec.sample_rate.get());
        let first_frame = (first.at * rate)
            .round()
            .to_usize()
            .expect("the start beat fits usize");
        let rebased: Vec<_> = raw
            .beats
            .iter()
            .filter(|beat| beat.ordinal >= first.ordinal)
            .map(|beat| kithara::beat::GridBeat {
                at: beat.at - first.at,
                ordinal: beat.ordinal - first.ordinal,
                confidence: beat.confidence,
            })
            .collect();
        let beats = rebased
            .iter()
            .map(|beat| {
                (
                    beat.ordinal.to_f64().expect("ordinal fits f64"),
                    beat.at * rate,
                )
            })
            .collect::<Vec<_>>();
        let needed = trajectories
            .iter()
            .map(|trajectory| {
                let anchors = anchors(trajectory, spec);
                source_end(name, &beats, last_host_beat(&anchors, trajectory, spec))
            })
            .max()
            .expect("a trajectory");
        let mut pcm = Vec::new();
        while pcm.len() < (first_frame + needed) * CH {
            match decoder
                .next_chunk()
                .unwrap_or_else(|error| panic!("decode `{name}`: {error:?}"))
            {
                DecoderChunkOutcome::Chunk(chunk) => pcm.extend_from_slice(&chunk.samples),
                DecoderChunkOutcome::Pending(reason) => panic!("`{name}` pending: {reason:?}"),
                DecoderChunkOutcome::Eof => panic!("`{name}` ends before frame {needed}"),
            }
        }
        pcm.truncate((first_frame + needed) * CH);
        pcm.drain(..first_frame * CH);
        let decoded = needed.to_f64().expect("frames fit f64");
        let model = kithara::beat::BeatGridModel::try_from(kithara::beat::RawBeatGrid {
            beats: rebased
                .into_iter()
                .filter(|beat| beat.at * rate < decoded)
                .collect(),
            downbeats: Vec::new(),
            meter: None,
            duration: None,
            ..raw.clone()
        })
        .unwrap_or_else(|error| panic!("rebase `{name}`'s grid: {error:?}"));
        Self {
            name,
            start,
            spec,
            pcm,
            beats,
            model,
            bpm: raw.bpm,
        }
    }

    /// The analysed beat ordinal at `frame` of the slice, between the two
    /// analysed beats around it.
    fn source_beat(&self, frame: f64) -> f64 {
        let after = self.beats.partition_point(|&(_, at)| at < frame);
        let (Some(&(start, from)), Some(&(end, to))) = (
            self.beats.get(after.saturating_sub(1)),
            self.beats.get(after),
        ) else {
            panic!("`{}` is analysed past frame {frame}", self.name);
        };
        if to <= from {
            return end;
        }
        start + (end - start) * (frame - from) / (to - from)
    }

    fn frames(&self) -> usize {
        self.pcm.len() / CH
    }

    fn source_grid(&self) -> BeatGridSnapshot {
        let axis = kithara::warp::AssetAxis::new(
            self.spec.sample_rate,
            kithara::warp::AssetExtent::Bounded(self.frames().to_u64().expect("frames fit u64")),
        );
        BeatGridSnapshot::model(
            BeatGridId::allocate().expect("grid id"),
            BeatGridRevision::first(),
            &self.model,
            axis,
        )
        .unwrap_or_else(|error| panic!("`{}`'s grid on its axis: {error:?}", self.name))
    }
}

/// The source frame of beat `ordinal` of `name`'s analysed `beats`, linear
/// between the beats around it as the grid's segments are.
fn source_frame(name: &str, beats: &[(f64, f64)], ordinal: f64) -> f64 {
    let after = beats.partition_point(|&(beat, _)| beat < ordinal);
    let (Some(&(start, from)), Some(&(end, to))) =
        (beats.get(after.saturating_sub(1)), beats.get(after))
    else {
        panic!("`{name}` is analysed past beat {ordinal}");
    };
    if end <= start {
        return to;
    }
    from + (to - from) * (ordinal - start) / (end - start)
}

/// The source frames a render reaching Host beat `last_beat` reads: a
/// retarget's transition consumes more source than the steady plan would.
fn source_end(name: &str, beats: &[(f64, f64)], last_beat: f64) -> usize {
    let margin = consts::SOURCE_MARGIN_BEATS
        .to_f64()
        .expect("margin fits f64");
    source_frame(name, beats, last_beat.ceil() + margin)
        .ceil()
        .to_usize()
        .expect("frame fits usize")
}

/// The Host beat `trajectory` reaches at its end.
fn last_host_beat(anchors: &[SessionAnchor], trajectory: &Trajectory, spec: AudioSpec) -> f64 {
    let frames = trajectory.seconds * usize::try_from(spec.sample_rate.get()).expect("rate");
    f64::from(
        anchors
            .last()
            .expect("an anchor")
            .beat_at(SessionFrame::new(i64::try_from(frames).expect("fits i64")))
            .expect("final Host beat"),
    )
}

/// The Host anchors of `trajectory`, one per committed retarget.
fn anchors(trajectory: &Trajectory, spec: AudioSpec) -> Vec<SessionAnchor> {
    let rate = usize::try_from(spec.sample_rate.get()).expect("rate fits usize");
    let base = SessionAnchor::new(
        SessionFrame::new(0),
        SessionBeat::default(),
        trajectory.start_bpm / consts::SECONDS_PER_MINUTE,
        spec.sample_rate,
    )
    .expect("Host anchor");
    let mut anchors: Vec<SessionAnchor> = Vec::new();
    for &(seconds, bpm) in trajectory.retargets {
        let from = anchors.last().copied().unwrap_or(base);
        let frame = SessionFrame::new(i64::try_from(seconds * rate).expect("frame fits i64"));
        let next = from
            .retarget(
                frame,
                bpm / consts::SECONDS_PER_MINUTE,
                trajectory.smooth_seconds,
            )
            .expect("Host retarget");
        anchors.push(next);
    }
    if anchors.is_empty() {
        anchors.push(base);
    }
    anchors
}

/// The session frame the Host plays `beat` at under `anchors`.
fn host_frame(anchors: &[SessionAnchor], beat: f64) -> f64 {
    let beat = SessionBeat::new(beat).expect("Host beat");
    let anchor = anchors
        .iter()
        .rev()
        .find(|anchor| anchor.beat() <= beat)
        .unwrap_or(&anchors[0]);
    i64::from(anchor.frame_at(beat).expect("Host beat frame"))
        .to_f64()
        .expect("frame fits f64")
}

/// The plan projecting the track onto `anchor`'s Host grid from
/// `activation`, as the sync owner freezes it when the Host commits
/// `anchor`: never before the output the member has already presented.
fn plan(
    source: &BeatGridSnapshot,
    anchor: SessionAnchor,
    activation: SessionFrame,
    revision: WarpMapRevision,
) -> Projection {
    let target = BeatGridSnapshot::session(
        BeatGridId::allocate().expect("grid id"),
        BeatGridRevision::first(),
        SessionEpoch::new(0),
        anchor,
        None,
    );
    let beat = Beat::new(f64::from(anchor.beat())).expect("anchor beat");
    let alignment = BeatAlignment::new(
        MapPoint::new(source.stamp(), beat),
        MapPoint::new(target.stamp(), beat),
    );
    let grid = BeatGridSnapshot::projection(source.clone(), target.clone(), alignment)
        .expect("projection");
    let map = WarpMap::projected(source.clone(), target, alignment, revision).expect("projection");
    (map, activation.max(anchor.frame()), source.clone(), grid)
}

fn render(
    track: &Track,
    backend: StretchKind,
    trajectory: &Trajectory,
    anchors: &[SessionAnchor],
) -> Presented {
    let frames = trajectory.seconds * usize::try_from(track.spec.sample_rate.get()).expect("rate");
    let source = track.source_grid();
    let mut revision = WarpMapRevision::first();
    let initial = plan(&source, anchors[0], anchors[0].frame(), revision);
    let mut next = 1;
    let mut update = |_: u64, output: usize| {
        let anchor = *anchors.get(next)?;
        let output = SessionFrame::new(i64::try_from(output).expect("frame fits i64"));
        if output < anchor.frame() {
            return None;
        }
        revision = revision.checked_next().expect("map revision");
        next += 1;
        Some((
            anchor,
            revision,
            Some(plan(&source, anchor, output, revision)),
        ))
    };
    let source_end = source_end(
        track.name,
        &track.beats,
        last_host_beat(anchors, trajectory, track.spec),
    );
    let mut rendered = render_configured_grid_with_updates(
        WarpConfig::builder()
            .speed(1.0)
            .keylock(true)
            .backend(backend)
            .render_quantum_frames(NonZero::new(64).expect("quantum"))
            .build(),
        Some(initial),
        InterleavedView::new(
            &track.pcm[..source_end * CH],
            track.spec,
            FrameCount::new(source_end),
        )
        .expect("decoded track is whole frames"),
        Timeline::Anchored(anchors[0]),
        None,
        Some(frames),
        &mut update,
    );
    assert_eq!(next, anchors.len(), "every retarget was applied");
    assert!(
        rendered.samples.len() >= frames * CH,
        "{}: {} of {frames} Host frames rendered",
        trajectory.name,
        rendered.samples.len() / CH
    );
    rendered.samples.truncate(frames * CH);
    rendered.positions.retain(|&(output, _)| output < frames);
    rendered
}

/// Every presented chunk whose published audible source frame is not where
/// the Host plays it: the build-time grid names the analysed beat at that
/// source frame, the Host anchors name the output frame that beat plays at,
/// and the chunk must have been presented there.
fn misplaced_positions(
    track: &Track,
    anchors: &[SessionAnchor],
    positions: &[(usize, u64)],
) -> Vec<String> {
    positions
        .iter()
        .filter_map(|&(output, source)| {
            let beat = track.source_beat(source.to_f64().expect("frame fits f64"));
            let expected = host_frame(anchors, beat);
            let error = output.to_f64().expect("frame fits f64") - expected;
            (error.abs() > consts::TOLERANCE_FRAMES).then(|| {
                format!(
                    "output frame {output}: source frame {source} is analysed beat {beat:.3}, \
                     which the Host plays at frame {expected:.0} ({error:+.0} frames)"
                )
            })
        })
        .collect()
}

/// The start as an artifact-label fragment: `bar0-beat1` for beat 1 of bar 0,
/// `beat64` for analysed beat 64, `5250ms` for 5.25 s.
fn start_label(start: Start) -> String {
    match start {
        Start::Bar { bar, beat } => format!("bar{bar}-beat{beat}"),
        Start::Beat(ordinal) => format!("beat{ordinal}"),
        Start::Seconds(seconds) => format!("{:.0}ms", seconds * 1_000.0),
    }
}

fn record(track: &Track, backend: StretchKind, trajectory: &Trajectory, output: &[f32]) {
    let Some(mut tap) = AudioArtifactTap::from_env(
        &format!(
            "warp-{}-{}-{backend:?}-{}",
            track.name,
            start_label(track.start),
            trajectory.name
        ),
        track.spec.sample_rate.get(),
        track.spec.channels,
    )
    .expect("listening artifact") else {
        return;
    };
    tap.push(output);
    tap.evidence(
        "projection",
        serde_json::json!({
            "track": track.name,
            "source_bpm": track.bpm,
            "start_bpm": trajectory.start_bpm,
            "retargets": trajectory.retargets,
            "smooth_seconds": trajectory.smooth_seconds,
        }),
    );
}

/// Tunnel opens on its kick; its grid states bars.
const TUNNEL: Library = Library {
    name: "library_mp3_zvuk_27390231",
    extension: "mp3",
};
/// Newtechno's grid states no bars: its detected downbeats disagree on the
/// bar phase. Its first phrase change lands on analysed beat 64.
const NEWTECHNO: Library = Library {
    name: "library_flac_newtechno",
    extension: "flac",
};
/// Newtechno's second phrase, where the full groove enters.
const NEWTECHNO_PHRASE: Start = Start::Beat(64);
/// An entry on the second beat of Tunnel's first bar.
const WEAK_BEAT: Start = Start::Bar { bar: 0, beat: 1 };

#[kithara::test(timeout(Duration::from_secs(600)))]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::tunnel_signalsmith(TUNNEL, Start::bar(0), StretchKind::Signalsmith)
)]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::newtechno_signalsmith(NEWTECHNO, NEWTECHNO_PHRASE, StretchKind::Signalsmith)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::tunnel_bungee(TUNNEL, Start::bar(0), StretchKind::Bungee)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::newtechno_bungee(NEWTECHNO, NEWTECHNO_PHRASE, StretchKind::Bungee)
)]
#[case::tunnel_glide(TUNNEL, Start::bar(0), StretchKind::Glide)]
#[case::newtechno_glide(NEWTECHNO, NEWTECHNO_PHRASE, StretchKind::Glide)]
#[case::tunnel_weak_beat_glide(TUNNEL, WEAK_BEAT, StretchKind::Glide)]
fn analysed_beats_land_on_the_host_beats_they_project_to(
    #[case] library: Library,
    #[case] start: Start,
    #[case] backend: StretchKind,
) {
    let name = library.name;
    let track = Track::load(library, start, TRAJECTORIES);
    let mut failures = Vec::new();
    for trajectory in TRAJECTORIES {
        let anchors = anchors(trajectory, track.spec);
        let rendered = render(&track, backend, trajectory, &anchors);
        record(&track, backend, trajectory, &rendered.samples);
        assert!(
            !rendered.positions.is_empty(),
            "{}: the render presented no chunk",
            trajectory.name
        );
        failures.extend(
            misplaced_positions(&track, &anchors, &rendered.positions)
                .into_iter()
                .map(|failure| format!("{}: {failure}", trajectory.name)),
        );
    }
    assert!(
        failures.is_empty(),
        "{name} ({backend:?}, {} BPM): presented source missed its Host beats:\n{}",
        track.bpm,
        failures.join("\n")
    );
}
