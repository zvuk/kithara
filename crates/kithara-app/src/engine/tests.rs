use std::{cell::Cell, convert::Infallible, num::NonZeroU16};

use ::kithara::{
    analysis::{
        BeatGridModel, BeatGridState, GRID_SCHEMA_VERSION, GridBeat, GridDownbeat, Meter,
        RawBeatGrid,
    },
    host::{HostSettingsControl, api::Tempo},
    output::{OfflineRenderRequest, OfflineRenderer, RenderSink, RenderSinkError},
    platform::{
        sync::Arc,
        thread,
        time::Duration,
        tokio::{
            sync::{mpsc, watch},
            task::{self, JoinHandle},
        },
    },
    queue::{TrackId, TrackStatus, Transition},
    signal::{AudioSpec, SessionFrame},
    ui::render::ControlAction,
    warp::{Beat, BeatGrid, BeatGridQuery, BeatGridSnapshot, MapPoint, MapPosition},
};
use kithara_test_utils::{kithara, off_thread::OffThread};
use num_traits::cast::AsPrimitive;

use super::owner::mock;
use crate::{
    analysis::{
        AnalysisHandle, Request, TrackArtifacts,
        fixtures::{self, constant_fifth_wav, constant_tenth_wav, rhythm_a_wav, rhythm_b_wav},
    },
    gui::rig::Rig,
};

mod consts {
    use super::Duration;

    pub(super) const BEATS_PER_BAR: u16 = 4;
    pub(super) const BLOCK_FRAMES: u64 = 1_024;
    pub(super) const CHANNELS: u16 = 2;
    pub(super) const CROSSFADE: Duration = Duration::from_secs(1);
    pub(super) const GAPLESS_SHARE: f64 = 0.99;
    pub(super) const GRID_ARRIVAL: Duration = Duration::from_millis(500);
    pub(super) const INCOMING_BEATS: usize = 8;
    pub(super) const LEVEL_TOLERANCE: f32 = 1e-3;
    pub(super) const MEASURED_BEATS: usize = 16;
    pub(super) const ONSET_THRESHOLD: f32 = 0.15;
    pub(super) const ONSET_TOLERANCE: Duration = Duration::from_millis(5);
    pub(super) const PACE: Duration = Duration::from_millis(3);
    pub(super) const PAUSE: Duration = Duration::from_millis(200);
    pub(super) const PLAY_ANSWER: Duration = Duration::from_secs(2);
    pub(super) const PULSE: Duration = Duration::from_millis(40);
    pub(super) const REFRACTORY: Duration = Duration::from_millis(100);
    pub(super) const RESUME_ANSWER: Duration = Duration::from_millis(250);
    pub(super) const RESUME_WINDOW: Duration = Duration::from_millis(2_500);
    pub(super) const REVISION_ANSWER: Duration = Duration::from_secs(2);
    pub(super) const SETTLE: Duration = Duration::from_secs(1);
    pub(super) const TRACK_BEATS: i64 = 23;
    pub(super) const TRACK_BPM: f64 = 120.0;
    pub(super) const TRACK_FIRST_BEAT: f64 = 0.5;
    pub(super) const TRACK_SECONDS: f64 = 12.0;
}

/// The grid of `rhythm_wav_deck_a_120bpm_48k`: a click every half second
/// from 0.5 s, every fourth one accented.
fn rhythm_grid() -> BeatGridModel {
    let period = 60.0 / consts::TRACK_BPM;
    let at = |ordinal: i64| {
        let ordinal = i32::try_from(ordinal).expect("fixture beat ordinal fits i32");
        f64::from(ordinal).mul_add(period, consts::TRACK_FIRST_BEAT)
    };
    BeatGridModel::try_from(RawBeatGrid {
        schema_version: GRID_SCHEMA_VERSION,
        model_id: "rhythm".to_owned(),
        revision: 1,
        state: BeatGridState::Final,
        duration: Some(consts::TRACK_SECONDS),
        bpm: consts::TRACK_BPM,
        beats: (0..consts::TRACK_BEATS)
            .map(|ordinal| GridBeat {
                at: at(ordinal),
                ordinal,
                confidence: Some(1.0),
            })
            .collect(),
        downbeats: (0..consts::TRACK_BEATS)
            .step_by(usize::from(consts::BEATS_PER_BAR))
            .map(|ordinal| GridDownbeat {
                at: at(ordinal),
                beat_ordinal: ordinal,
                confidence: Some(1.0),
            })
            .collect(),
        meter: Some(Meter {
            beats_per_bar: NonZeroU16::new(consts::BEATS_PER_BAR).expect("a bar holds beats"),
            origin_beat_ordinal: 0,
        }),
    })
    .expect("the rhythm grid holds together")
}

/// A grid no host tempo near the fixture's can reach: a beat every 15 s.
fn unreachable_grid() -> BeatGridModel {
    let mut raw = rhythm_grid().as_raw().clone();
    raw.bpm = 4.0;
    raw.duration = Some(40.0);
    raw.beats = (0..3)
        .map(|ordinal| GridBeat {
            at: f64::from(i32::try_from(ordinal).expect("a small ordinal")) * 15.0,
            ordinal,
            confidence: Some(1.0),
        })
        .collect();
    raw.downbeats.truncate(1);
    for downbeat in &mut raw.downbeats {
        downbeat.at = 0.0;
    }
    BeatGridModel::try_from(raw).expect("the slow grid holds together")
}

/// The rhythm grid with a last beat early, so its last interval plays faster
/// than the rest of the track.
fn grid_with_an_early_last_beat() -> BeatGridModel {
    let mut raw = rhythm_grid().as_raw().clone();
    let last = raw.beats.last_mut().expect("the rhythm grid holds beats");
    last.at -= 0.1;
    raw.duration = Some(last.at);
    BeatGridModel::try_from(raw).expect("the grid holds together")
}

/// The rhythm grid as an analysis measured it past the deck's declared
/// duration: its last beat lies after that duration.
fn grid_past_the_end() -> BeatGridModel {
    let mut raw = rhythm_grid().as_raw().clone();
    let period = 60.0 / consts::TRACK_BPM;
    raw.beats.push(GridBeat {
        at: consts::TRACK_SECONDS + period,
        ordinal: consts::TRACK_BEATS + 1,
        confidence: Some(1.0),
    });
    raw.duration = Some(consts::TRACK_SECONDS + 2.0 * period);
    BeatGridModel::try_from(raw).expect("the extended grid holds together")
}

/// The rhythm grid as an early analysis hears it: every beat a quarter beat late.
fn grid_a_quarter_beat_late() -> BeatGridModel {
    let mut raw = rhythm_grid().as_raw().clone();
    let shift = 0.25 * 60.0 / consts::TRACK_BPM;
    raw.state = BeatGridState::Provisional;
    for beat in &mut raw.beats {
        beat.at += shift;
    }
    for downbeat in &mut raw.downbeats {
        downbeat.at += shift;
    }
    BeatGridModel::try_from(raw).expect("the shifted grid holds together")
}

/// The rhythm grid as an early analysis hears it: its bars start two beats late.
fn grid_two_beats_off_the_bar() -> BeatGridModel {
    let mut raw = rhythm_grid().as_raw().clone();
    raw.state = BeatGridState::Provisional;
    raw.meter = Some(Meter {
        beats_per_bar: NonZeroU16::new(consts::BEATS_PER_BAR).expect("a bar holds beats"),
        origin_beat_ordinal: 2,
    });
    raw.downbeats = raw
        .beats
        .iter()
        .skip(2)
        .step_by(usize::from(consts::BEATS_PER_BAR))
        .map(|beat| GridDownbeat {
            at: beat.at,
            beat_ordinal: beat.ordinal,
            confidence: Some(1.0),
        })
        .collect();
    BeatGridModel::try_from(raw).expect("the misplaced bars hold together")
}

#[derive(Default)]
struct Capture(Vec<f32>);

impl RenderSink for Capture {
    fn write(&mut self, samples: &[f32]) -> Result<(), RenderSinkError> {
        self.0.extend_from_slice(samples);
        Ok(())
    }
}

/// Host output the engine loop rendered.
struct Rendered {
    /// The first session frame.
    start: i64,
    /// Interleaved samples.
    pcm: Vec<f32>,
}

/// A rig with SYNC pressed on deck A, and the task serving its analysis.
struct Synced<T> {
    rig: OffThread<Rig>,
    served: JoinHandle<T>,
}

/// Renders `length` of host output as the engine loop would.
fn capture(rig: &mut Rig, length: Duration) -> Rendered {
    capture_paced(rig, length, consts::PACE)
}

/// Renders `length` of host output, one block per `pace` of wall time.
fn capture_paced(rig: &mut Rig, length: Duration, pace: Duration) -> Rendered {
    let start = rig.rendered_frames;
    let end = start + frames(rig, length);
    let mut sink = Capture::default();
    let cancel = rig.shutdown.clone();
    while rig.rendered_frames < end {
        let from = rig.rendered_frames;
        let to = (from + consts::BLOCK_FRAMES).min(end);
        let host = mock::host(rig);
        let request = OfflineRenderRequest::builder()
            .spec(AudioSpec::new(consts::CHANNELS, host.sample_rate()))
            .frames(from..to)
            .build();
        host.render(&request, &cancel, &mut sink)
            .expect("the offline app output renders");
        rig.rendered_frames = to;
        rig.engine.tick();
        rig.engine.publish();
        thread::sleep(pace);
    }
    Rendered {
        start: i64::try_from(start).expect("rendered frames fit the session axis"),
        pcm: sink.0,
    }
}

fn frames(rig: &mut Rig, length: Duration) -> u64 {
    let rate = mock::host(rig).sample_rate().get();
    (length.as_secs_f64() * f64::from(rate)).round().as_()
}

#[derive(Debug)]
struct Onset {
    frame: i64,
    energy: f32,
}

/// Click onsets: a threshold crossing after a pulse of quiet.
fn onsets(rig: &mut Rig, start: i64, pcm: &[f32]) -> Vec<Onset> {
    let level: Vec<f32> = pcm
        .chunks_exact(usize::from(consts::CHANNELS))
        .map(|frame| {
            frame
                .iter()
                .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
        })
        .collect();
    let threshold = consts::ONSET_THRESHOLD * level.iter().copied().fold(0.0, f32::max);
    let pulse = usize::try_from(frames(rig, consts::PULSE)).expect("pulse fits usize");
    let refractory =
        usize::try_from(frames(rig, consts::REFRACTORY)).expect("refractory fits usize");
    let mut found = Vec::new();
    let mut quiet = 0;
    let mut at = 0;
    while at < level.len() {
        if level[at] <= threshold {
            quiet += 1;
            at += 1;
            continue;
        }
        if quiet >= pulse {
            let energy = level[at..level.len().min(at + pulse)]
                .iter()
                .map(|sample| sample * sample)
                .sum();
            found.push(Onset {
                frame: start + i64::try_from(at).expect("capture offset fits i64"),
                energy,
            });
            at += refractory;
        } else {
            at += 1;
        }
        quiet = 0;
    }
    found
}

fn host_beat(grid: &BeatGridSnapshot, frame: i64) -> f64 {
    let query = grid.beat_at(MapPoint::new(
        grid.stamp(),
        MapPosition::Session(SessionFrame::new(frame)),
    ));
    let BeatGridQuery::Resolved(beat) = query else {
        panic!("the host grid resolves frame {frame}: {query:?}");
    };
    f64::from(*beat.value().value())
}

fn beat_frame(grid: &BeatGridSnapshot, beat: f64) -> i64 {
    let query = grid.position_at(MapPoint::new(
        grid.stamp(),
        Beat::new(beat).expect("finite host beat"),
    ));
    let BeatGridQuery::Resolved(position) = query else {
        panic!("the host grid resolves beat {beat}: {query:?}");
    };
    let MapPosition::Session(at) = *position.value().value() else {
        panic!("host beats sit on the session axis");
    };
    i64::from(at)
}

/// Deck A plays the first of `urls` with the analysis `serve` answers and the
/// host at `host_bpm`, then SYNC is pressed.
async fn synced<T: Send + 'static>(
    urls: Vec<String>,
    host_bpm: f64,
    serve: impl FnOnce(mpsc::Receiver<Request>, Vec<TrackId>) -> JoinHandle<T>,
) -> Synced<T> {
    let (analysis, requests) = AnalysisHandle::channel(watch::channel(Arc::default()).1);
    let rig = OffThread::spawn("engine", move || {
        Ok::<_, Infallible>(Rig::playback_with(analysis))
    })
    .await
    .expect("rig fixture is infallible");
    let tracks = rig
        .call(move |rig| {
            urls.iter()
                .map(|url| {
                    rig.queues[0]
                        .append(url.as_str())
                        .expect("deck A takes the track")
                })
                .collect()
        })
        .await;
    let served = serve(requests, tracks);
    rig.call(move |rig| {
        rig.send("deck-a/play", ControlAction::Activate);
        rig.pump();
        rig.until(
            "deck A plays the track with its grid",
            Rig::DEADLINE,
            Rig::frame,
            |rig| {
                rig.queues[0].is_playing()
                    && rig.text("deck.playback.bpm@deck=a").as_deref() == Some("120.0")
            },
        );
        let tempo = Tempo::new(host_bpm).expect("fixture tempo is in range");
        mock::host(rig)
            .set_tempo(tempo)
            .expect("the host takes the tempo");
        rig.until(
            &format!("the host confirms {host_bpm} BPM"),
            Rig::DEADLINE,
            Rig::frame,
            |rig| mock::host(rig).tempo() == tempo,
        );
        rig.send("deck-a/sync", ControlAction::Activate);
        rig.pump();
    })
    .await;
    Synced { rig, served }
}

/// Deck A plays the rhythm track `url` with its grid served.
async fn synced_one(url: String, host_bpm: f64) -> Synced<watch::Sender<Option<TrackArtifacts>>> {
    synced(vec![url], host_bpm, |requests, tracks| {
        task::spawn(fixtures::serve_grid(requests, tracks[0], rhythm_grid()))
    })
    .await
}

/// Answers every subscription for a track from that track's own channel.
async fn serve_tracks(
    mut requests: mpsc::Receiver<Request>,
    channels: Vec<(TrackId, watch::Sender<Option<TrackArtifacts>>)>,
) {
    while let Some(request) = requests.recv().await {
        if let Request::Subscribe {
            track_id, reply, ..
        } = request
        {
            let (_, channel) = channels
                .iter()
                .find(|(id, _)| *id == track_id)
                .expect("the deck asks for a track it was given");
            let _ = reply.send(channel.subscribe());
        }
    }
}

/// Captures `beats` host beats at `host_bpm` and asserts
/// that deck A plays through them with every click on a host beat and the
/// accented clicks on the host downbeats.
fn assert_on_host_beats(rig: &mut Rig, host_bpm: f64, beats: usize) {
    let period = Duration::from_secs_f64(60.0 / host_bpm);
    let measured = period * u32::try_from(beats).expect("beat count fits u32");
    let played = rig.queues[0].position_seconds();
    let Rendered { start, pcm } = capture(rig, measured);
    let played = rig.queues[0]
        .position_seconds()
        .zip(played)
        .map(|(to, from)| to - from);
    assert!(
        played.is_some_and(|played| played >= consts::GAPLESS_SHARE * measured.as_secs_f64()),
        "deck A plays through the capture of {measured:?}: {played:?}"
    );
    let found = onsets(rig, start, &pcm);
    assert!(
        found.len() >= beats - 1,
        "the capture holds the clicks: {found:?}"
    );

    let grid = mock::host(rig).snapshot();
    let tolerance =
        i64::try_from(frames(rig, consts::ONSET_TOLERANCE)).expect("tolerance fits i64");
    let bar = i64::from(consts::BEATS_PER_BAR);
    let mut loudness = vec![(0.0_f32, 0_u16); usize::from(consts::BEATS_PER_BAR)];
    let mut off_beat = Vec::new();
    for onset in &found {
        let beat = host_beat(&grid, onset.frame).round();
        let offset = onset.frame - beat_frame(&grid, beat);
        if offset.abs() > tolerance {
            off_beat.push((onset.frame, beat, offset));
        }
        let slot = usize::try_from(AsPrimitive::<i64>::as_(beat).rem_euclid(bar))
            .expect("a bar slot fits usize");
        loudness[slot].0 += onset.energy;
        loudness[slot].1 += 1;
    }
    assert!(
        off_beat.is_empty(),
        "clicks off the host beats (frame, host beat, offset): {off_beat:?}"
    );
    let mean: Vec<f32> = loudness
        .iter()
        .map(|(sum, count)| sum / f32::from((*count).max(1)))
        .collect();
    let loudest = mean
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(slot, _)| slot);
    assert_eq!(
        loudest,
        Some(0),
        "the accented clicks fall on the host downbeats, mean energy per bar slot: {mean:?}"
    );
}

#[kithara::test(native, tokio, flash(false))]
async fn sync_puts_the_deck_on_the_host_beat(rhythm_a_wav: String) {
    let Synced { rig, served } = synced_one(rhythm_a_wav, 124.0).await;
    rig.call(|rig| {
        let _ = capture(rig, consts::SETTLE);
        assert_on_host_beats(rig, 124.0, consts::MEASURED_BEATS);
    })
    .await;
    rig.close().await;
    drop(served);
}

/// The deck shows the seek's target, then plays on from it.
#[cfg(not(feature = "broadcast"))]
#[kithara::test(native, tokio, flash(false))]
async fn a_seek_on_a_synced_deck_shows_the_position_moving_on(rhythm_a_wav: String) {
    let Synced { rig, served } = synced_one(rhythm_a_wav, 124.0).await;
    rig.call(|rig| {
        let _ = capture(rig, consts::SETTLE);
        let duration = rig.queues[0]
            .duration_seconds()
            .expect("the playing deck knows its duration");
        let target = duration * 0.25;
        rig.send("deck-a/wave", ControlAction::SetScalar(0.25));
        rig.pump();
        rig.until(
            "the deck shows the seek",
            Rig::DEADLINE,
            Rig::frame,
            |rig| (rig.scalar("deck.playback.position_secs@deck=a") - target).abs() < 0.2,
        );
        rig.until(
            "the shown position plays on past the target",
            Rig::DEADLINE,
            Rig::frame,
            |rig| rig.scalar("deck.playback.position_secs@deck=a") > target + 0.3,
        );
    })
    .await;
    rig.close().await;
    drop(served);
}

/// A refused SYNC leaves the deck free: a track it takes afterwards takes a speed.
#[kithara::test(native, tokio, flash(false))]
async fn a_refused_sync_leaves_a_later_track_free(rhythm_a_wav: String, rhythm_b_wav: String) {
    let (analysis, mut requests) = AnalysisHandle::channel(watch::channel(Arc::default()).1);
    let rig = OffThread::spawn("engine", move || {
        Ok::<_, Infallible>(Rig::playback_with(analysis))
    })
    .await
    .expect("rig fixture is infallible");
    let first = rig
        .call(move |rig| {
            rig.queues[0]
                .append(rhythm_a_wav.as_str())
                .expect("deck A takes the track")
        })
        .await;
    let served = task::spawn(async move {
        let (slow, _) = watch::channel(Some(fixtures::supplied(unreachable_grid())));
        let (later, _) = watch::channel(Some(fixtures::supplied(rhythm_grid())));
        while let Some(request) = requests.recv().await {
            if let Request::Subscribe {
                track_id, reply, ..
            } = request
            {
                let channel = if track_id == first { &slow } else { &later };
                let _ = reply.send(channel.subscribe());
            }
        }
    });
    rig.call(move |rig| {
        rig.send("deck-a/play", ControlAction::Activate);
        rig.pump();
        rig.until(
            "deck A plays the track with its grid",
            Rig::DEADLINE,
            Rig::frame,
            |rig| {
                rig.queues[0].is_playing()
                    && rig.text("deck.playback.bpm@deck=a").as_deref() == Some("4.0")
            },
        );
        rig.send("deck-a/sync", ControlAction::Activate);
        rig.pump();
        rig.frame();
        assert!(
            !rig.flag("deck.playback.synced@deck=a"),
            "the host refuses SYNC on a tempo it cannot reach"
        );
        rig.queues[0]
            .append(rhythm_b_wav.as_str())
            .expect("deck A takes the later track");
        rig.send("deck-a/next", ControlAction::Activate);
        rig.pump();
        rig.until(
            "deck A moves to the later track",
            Rig::DEADLINE,
            Rig::frame,
            |rig| rig.queues[0].current_index() == Some(1),
        );
        let _ = capture(rig, consts::SETTLE);
        assert!(
            rig.queues[0].set_rate(1.0).is_ok(),
            "the later track inherits the SYNC the host refused"
        );
    })
    .await;
    rig.close().await;
    served.abort();
}

/// A refusal the host answers after SYNC was accepted shows on the deck: the
/// indicator goes off and the tempo fader works again.
#[kithara::test(native, tokio, flash(false))]
async fn a_grid_refusal_after_sync_shows_sync_off_and_unlocks_the_tempo(rhythm_a_wav: String) {
    let Synced { rig, served } = synced_one(rhythm_a_wav, 126.0).await;
    let analysis = served.await.expect("the analysis fixture serves");
    rig.call(|rig| {
        let _ = capture(rig, consts::SETTLE);
        rig.frame();
        assert!(rig.flag("deck.playback.synced@deck=a"));
        assert_eq!(
            rig.text("deck.playback.tempo@deck=a").as_deref(),
            Some("+5.0%")
        );
    })
    .await;
    analysis.send_replace(Some(fixtures::refused()));
    rig.call(|rig| {
        rig.until(
            "the refusal turns the SYNC indicator off",
            Rig::DEADLINE,
            Rig::frame,
            |rig| !rig.flag("deck.playback.synced@deck=a"),
        );
        rig.send("deck-a/tempo", ControlAction::StepScalar(1.0));
        rig.pump();
        rig.until(
            "the tempo fader steps from the played speed",
            Rig::DEADLINE,
            Rig::frame,
            |rig| rig.text("deck.playback.tempo@deck=a").as_deref() == Some("+6.5%"),
        );
    })
    .await;
    rig.close().await;
    drop(analysis);
}

/// A grid that arrives after a refusal for lack of one puts the track back on
/// the host at the tempo the host holds by then.
#[kithara::test(native, tokio, flash(false))]
async fn a_ready_grid_after_a_refusal_turns_sync_back_on_at_the_host_tempo(rhythm_a_wav: String) {
    let Synced { rig, served } = synced_one(rhythm_a_wav, 126.0).await;
    let analysis = served.await.expect("the analysis fixture serves");
    rig.call(|rig| {
        rig.until("deck A turns SYNC on", Rig::DEADLINE, Rig::frame, |rig| {
            rig.flag("deck.playback.synced@deck=a")
        });
    })
    .await;
    analysis.send_replace(Some(fixtures::refused()));
    rig.call(|rig| {
        rig.until(
            "the refusal turns the SYNC indicator off",
            Rig::DEADLINE,
            Rig::frame,
            |rig| !rig.flag("deck.playback.synced@deck=a"),
        );
        let tempo = Tempo::new(124.0).expect("fixture tempo is in range");
        mock::host(rig)
            .set_tempo(tempo)
            .expect("the host takes the tempo");
        rig.until(
            "the host confirms 124 BPM",
            Rig::DEADLINE,
            Rig::frame,
            |rig| mock::host(rig).tempo() == tempo,
        );
    })
    .await;
    analysis.send_replace(Some(fixtures::supplied(rhythm_grid())));
    rig.call(|rig| {
        rig.until(
            "the grid restores SYNC at the host tempo",
            Rig::DEADLINE,
            Rig::frame,
            |rig| {
                rig.flag("deck.playback.synced@deck=a")
                    && rig.text("deck.playback.tempo@deck=a").as_deref() == Some("+3.3%")
            },
        );
    })
    .await;
    rig.close().await;
    drop(analysis);
}

/// A SYNC press on a deck the refusal turned off shows SYNC on while the deck
/// waits for its grid.
#[kithara::test(native, tokio, flash(false))]
async fn a_sync_press_after_a_refusal_shows_sync_on_before_a_grid_arrives(rhythm_a_wav: String) {
    let Synced { rig, served } = synced_one(rhythm_a_wav, 126.0).await;
    let analysis = served.await.expect("the analysis fixture serves");
    rig.call(|rig| {
        rig.until("deck A turns SYNC on", Rig::DEADLINE, Rig::frame, |rig| {
            rig.flag("deck.playback.synced@deck=a")
        });
    })
    .await;
    analysis.send_replace(Some(fixtures::refused()));
    rig.call(|rig| {
        rig.until(
            "the refusal turns the SYNC indicator off",
            Rig::DEADLINE,
            Rig::frame,
            |rig| !rig.flag("deck.playback.synced@deck=a"),
        );

        rig.send("deck-a/sync", ControlAction::Activate);
        rig.pump();

        rig.until(
            "the press shows SYNC on before a grid arrives",
            Rig::DEADLINE,
            Rig::frame,
            |rig| rig.flag("deck.playback.synced@deck=a"),
        );
    })
    .await;
    rig.close().await;
    drop(analysis);
}

/// A tempo the host takes while the current track is refused reaches the
/// deck's next track, which sounds on the host beats of that tempo.
#[kithara::test(native, tokio, flash(false))]
async fn a_tempo_during_a_refusal_reaches_the_next_track(
    rhythm_a_wav: String,
    rhythm_b_wav: String,
) {
    let grid = || Some(fixtures::supplied(rhythm_grid()));
    let (current, _) = watch::channel(grid());
    let refusal = current.clone();
    let Synced { rig, served } = synced(
        vec![rhythm_a_wav, rhythm_b_wav],
        126.0,
        move |requests, tracks| {
            let (next, _) = watch::channel(grid());
            task::spawn(serve_tracks(
                requests,
                vec![(tracks[0], current), (tracks[1], next)],
            ))
        },
    )
    .await;
    rig.call(|rig| {
        rig.until("deck A turns SYNC on", Rig::DEADLINE, Rig::frame, |rig| {
            rig.flag("deck.playback.synced@deck=a")
        });
    })
    .await;
    refusal.send_replace(Some(fixtures::refused()));
    rig.call(|rig| {
        rig.until(
            "the refusal turns the SYNC indicator off",
            Rig::DEADLINE,
            Rig::frame,
            |rig| !rig.flag("deck.playback.synced@deck=a"),
        );
        let tempo = Tempo::new(124.0).expect("fixture tempo is in range");
        mock::host(rig)
            .set_tempo(tempo)
            .expect("the host takes the tempo");
        rig.until(
            "the host confirms 124 BPM",
            Rig::DEADLINE,
            Rig::frame,
            |rig| mock::host(rig).tempo() == tempo,
        );
        let _ = capture(rig, consts::SETTLE);
        rig.send("deck-a/next", ControlAction::Activate);
        rig.pump();
        rig.until(
            "deck A crossfades into its next track",
            Rig::DEADLINE,
            Rig::frame,
            |rig| rig.queues[0].current_index() == Some(1),
        );
        let _ = capture(rig, consts::CROSSFADE);
        assert_on_host_beats(rig, 124.0, consts::INCOMING_BEATS);
    })
    .await;
    rig.close().await;
    served.abort();
}

/// A playing deck whose grid runs past its duration turns SYNC on.
#[kithara::test(native, tokio, flash(false))]
async fn sync_on_a_playing_deck_with_a_grid_past_its_end(rhythm_a_wav: String) {
    let Synced { rig, served } = synced(vec![rhythm_a_wav], 124.0, |requests, tracks| {
        task::spawn(fixtures::serve_grid(
            requests,
            tracks[0],
            grid_past_the_end(),
        ))
    })
    .await;
    rig.call(|rig| {
        rig.until("deck A turns SYNC on", Rig::DEADLINE, Rig::frame, |rig| {
            rig.flag("deck.playback.synced@deck=a")
        });
        let _ = capture(rig, consts::SETTLE);
        assert_on_host_beats(rig, 124.0, consts::MEASURED_BEATS);
    })
    .await;
    rig.close().await;
    drop(served);
}

/// A synced deck crossfades from one rhythm track into the next, whose grid
/// the analysis holds when that track loads (`ready`) or publishes only once
/// the crossfade was asked for.
async fn crossfade_into_the_next_track(first: String, second: String, ready: bool) {
    let grid = || Some(fixtures::supplied(rhythm_grid()));
    let (next, _) = watch::channel(if ready { grid() } else { None });
    let served_next = next.clone();
    let Synced { rig, served } = synced(vec![first, second], 124.0, move |requests, tracks| {
        let (current, _) = watch::channel(grid());
        task::spawn(serve_tracks(
            requests,
            vec![(tracks[0], current), (tracks[1], served_next)],
        ))
    })
    .await;
    rig.call(move |rig| {
        let _ = capture(rig, consts::SETTLE);
        rig.send("deck-a/next", ControlAction::Activate);
        rig.pump();
        if !ready {
            let _ = capture(rig, consts::PULSE);
            next.send_replace(grid());
        }
        rig.until(
            "deck A crossfades into its next track",
            Rig::DEADLINE,
            Rig::frame,
            |rig| rig.queues[0].current_index() == Some(1),
        );
        let _ = capture(rig, consts::CROSSFADE);
        assert_on_host_beats(rig, 124.0, consts::INCOMING_BEATS);
    })
    .await;
    rig.close().await;
    served.abort();
}

/// A synced deck sounding on the grid `early` hears the rhythm track on the
/// host beats soon after the true grid replaces it.
async fn a_revision_lands_on_the_host_beat(url: String, early: BeatGridModel) {
    let revised = watch::channel(Some(fixtures::supplied(early))).0;
    let served_grid = revised.clone();
    let Synced { rig, served } = synced(vec![url], 124.0, move |requests, tracks| {
        task::spawn(serve_tracks(requests, vec![(tracks[0], served_grid)]))
    })
    .await;
    rig.call(move |rig| {
        rig.until("deck A turns SYNC on", Rig::DEADLINE, Rig::frame, |rig| {
            rig.flag("deck.playback.synced@deck=a")
        });
        let _ = capture(rig, consts::SETTLE);
        revised.send_replace(Some(fixtures::supplied(rhythm_grid())));
        let _ = capture(rig, consts::REVISION_ANSWER);
        assert_on_host_beats(rig, 124.0, consts::INCOMING_BEATS);
    })
    .await;
    rig.close().await;
    served.abort();
}

#[kithara::test(native, tokio, flash(false))]
async fn a_revision_by_a_quarter_beat_lands_on_the_host_beat(rhythm_a_wav: String) {
    a_revision_lands_on_the_host_beat(rhythm_a_wav, grid_a_quarter_beat_late()).await;
}

#[kithara::test(native, tokio, flash(false))]
async fn a_revision_of_the_bar_lands_on_the_host_downbeat(rhythm_a_wav: String) {
    a_revision_lands_on_the_host_beat(rhythm_a_wav, grid_two_beats_off_the_bar()).await;
}

/// A synced deck changes to a track the analysis has heard only roughly, and
/// the true grid arrives once that track sounds.
#[kithara::test(native, tokio, flash(false))]
async fn a_track_change_into_a_track_analysed_late_ends_on_the_host_beat(
    rhythm_a_wav: String,
    rhythm_b_wav: String,
) {
    let next = watch::channel(Some(fixtures::supplied(grid_a_quarter_beat_late()))).0;
    let served_next = next.clone();
    let urls = vec![rhythm_a_wav, rhythm_b_wav];
    let Synced { rig, served } = synced(urls, 124.0, move |requests, tracks| {
        let current = watch::channel(Some(fixtures::supplied(rhythm_grid()))).0;
        task::spawn(serve_tracks(
            requests,
            vec![(tracks[0], current), (tracks[1], served_next)],
        ))
    })
    .await;
    rig.call(move |rig| {
        let _ = capture(rig, consts::SETTLE);
        rig.send("deck-a/next", ControlAction::Activate);
        rig.pump();
        rig.until(
            "deck A crossfades into its next track",
            Rig::DEADLINE,
            Rig::frame,
            |rig| rig.queues[0].current_index() == Some(1),
        );
        next.send_replace(Some(fixtures::supplied(rhythm_grid())));
        let _ = capture(rig, consts::REVISION_ANSWER);
        assert_on_host_beats(rig, 124.0, consts::INCOMING_BEATS);
    })
    .await;
    rig.close().await;
    served.abort();
}

/// The level a capture holds on every sample, if it holds one.
fn plateau(rig: &mut Rig) -> Option<f32> {
    let Rendered { pcm, .. } = capture(rig, consts::PULSE);
    let (first, rest) = pcm.split_first()?;
    rest.iter()
        .all(|sample| (sample - first).abs() < consts::LEVEL_TOLERANCE)
        .then_some(*first)
}

/// Deck A plays the first of `urls`, synced, with every track's grid served.
async fn synced_tracks(urls: Vec<String>) -> Synced<()> {
    synced(urls, consts::TRACK_BPM, |requests, tracks| {
        let answer = |track| {
            (
                track,
                watch::channel(Some(fixtures::supplied(rhythm_grid()))).0,
            )
        };
        task::spawn(serve_tracks(
            requests,
            tracks.into_iter().map(answer).collect(),
        ))
    })
    .await
}

/// The deck, holding one level, sounds twice that level after `change`.
fn assert_doubles(rig: &mut Rig, what: &str, change: impl FnOnce(&mut Rig)) {
    let before = plateau(rig).expect("the deck holds one level");
    change(rig);
    let level = Cell::new(None);
    let doubled = |level: f32| (level - 2.0 * before).abs() < consts::LEVEL_TOLERANCE;
    rig.until(
        what,
        Rig::DEADLINE,
        |rig| level.set(plateau(rig)),
        |_| level.get().is_some_and(doubled),
    );
}

/// A synced deck goes back to the track it left, which the list holds before
/// the current one.
#[kithara::test(native, tokio, flash(false))]
async fn a_synced_deck_sounds_the_previous_track(
    constant_fifth_wav: String,
    constant_tenth_wav: String,
) {
    let Synced { rig, served } = synced_tracks(vec![constant_fifth_wav, constant_tenth_wav]).await;
    rig.call(|rig| {
        rig.send("deck-a/next", ControlAction::Activate);
        rig.pump();
        rig.until(
            "deck A moves to the next track",
            Rig::DEADLINE,
            Rig::frame,
            |rig| rig.queues[0].current_index() == Some(1),
        );
        let _ = capture(rig, consts::CROSSFADE);
        assert_doubles(rig, "deck A sounds the previous track", |rig| {
            rig.send("deck-a/prev", ControlAction::Activate);
            rig.pump();
        });
    })
    .await;
    rig.close().await;
    served.abort();
}

/// A synced deck is sent to an already loaded track, which the list holds past
/// another loaded track.
#[kithara::test(native, tokio, flash(false))]
async fn a_synced_deck_sounds_a_track_past_a_loaded_one(
    constant_fifth_wav: String,
    constant_tenth_wav: String,
) {
    let urls = vec![
        constant_tenth_wav.clone(),
        constant_tenth_wav,
        constant_fifth_wav,
    ];
    let Synced { rig, served } = synced_tracks(urls).await;
    rig.call(|rig| {
        rig.until(
            "deck A loads every track",
            Rig::DEADLINE,
            Rig::frame,
            |rig| {
                rig.queues[0]
                    .tracks()
                    .iter()
                    .skip(1)
                    .all(|track| track.status == TrackStatus::Loaded)
            },
        );
        assert_doubles(rig, "deck A sounds the selected track", |rig| {
            let last = rig.queues[0].tracks()[2].id;
            rig.queues[0]
                .select(last, Transition::Crossfade)
                .expect("deck A takes the selection");
        });
    })
    .await;
    rig.close().await;
    served.abort();
}

/// The loudest sample of `length` of host output.
fn peak(rig: &mut Rig, length: Duration) -> f32 {
    let Rendered { pcm, .. } = capture(rig, length);
    pcm.iter().fold(0.0, |peak, sample| peak.max(sample.abs()))
}

/// A pause on a synced deck also holds the track it was sent to, which waits
/// for a grid.
#[kithara::test(native, tokio, flash(false))]
async fn a_paused_synced_deck_stays_silent_when_its_selected_track_gets_a_grid(
    constant_fifth_wav: String,
    constant_tenth_wav: String,
) {
    let next = watch::channel(None).0;
    let withheld = next.clone();
    let urls = vec![constant_tenth_wav, constant_fifth_wav];
    let Synced { rig, served } = synced(urls, consts::TRACK_BPM, move |requests, tracks| {
        let current = watch::channel(Some(fixtures::supplied(rhythm_grid()))).0;
        task::spawn(serve_tracks(
            requests,
            vec![(tracks[0], current), (tracks[1], withheld)],
        ))
    })
    .await;
    rig.call(move |rig| {
        rig.until(
            "deck A loads the next track",
            Rig::DEADLINE,
            Rig::frame,
            |rig| rig.queues[0].tracks()[1].status == TrackStatus::Loaded,
        );
        let selected = rig.queues[0].tracks()[1].id;
        rig.queues[0]
            .select(selected, Transition::Crossfade)
            .expect("deck A takes the selection");
        let _ = capture(rig, consts::CROSSFADE);
        assert_eq!(
            rig.queues[0].current_index(),
            Some(0),
            "the selected track waits for its grid"
        );
        assert!(
            peak(rig, consts::PULSE) > consts::LEVEL_TOLERANCE,
            "deck A sounds until the pause"
        );
        rig.send("deck-a/play", ControlAction::Activate);
        rig.pump();
        let _ = capture(rig, consts::PULSE);
        let paused = peak(rig, consts::PULSE);
        assert!(
            paused < consts::LEVEL_TOLERANCE,
            "the pause silences deck A, peak {paused}"
        );
        next.send_replace(Some(fixtures::supplied(rhythm_grid())));
        let after = peak(rig, consts::GRID_ARRIVAL);
        assert!(
            after < consts::LEVEL_TOLERANCE,
            "the grid starts nothing on a paused deck, peak {after}"
        );
    })
    .await;
    rig.close().await;
    served.abort();
}

/// A play taking back a pause leaves the deck on its way to the selected track.
#[kithara::test(native, tokio, flash(false))]
async fn a_play_taking_back_a_pause_still_sounds_the_selected_track(
    constant_fifth_wav: String,
    constant_tenth_wav: String,
) {
    let Synced { rig, served } = synced_tracks(vec![constant_tenth_wav, constant_fifth_wav]).await;
    rig.call(|rig| {
        assert_doubles(rig, "deck A sounds the selected track", |rig| {
            let selected = rig.queues[0].tracks()[1].id;
            rig.queues[0]
                .select(selected, Transition::Crossfade)
                .expect("deck A takes the selection");
            rig.queues[0].pause();
            rig.queues[0].play();
        });
    })
    .await;
    rig.close().await;
    served.abort();
}

/// Play on a paused synced deck whose position lies before the first beat
/// starts from that beat.
#[cfg(not(feature = "broadcast"))]
#[kithara::test(native, tokio, flash(false))]
async fn play_on_a_synced_deck_paused_before_the_first_beat_sounds(rhythm_a_wav: String) {
    let Synced { rig, served } = synced_one(rhythm_a_wav, consts::TRACK_BPM).await;
    rig.call(|rig| {
        rig.until("deck A turns SYNC on", Rig::DEADLINE, Rig::frame, |rig| {
            rig.flag("deck.playback.synced@deck=a")
        });
        rig.send("deck-a/play", ControlAction::Activate);
        rig.pump();
        let _ = capture(rig, consts::PULSE);
        rig.send("deck-a/wave", ControlAction::SetScalar(0.01));
        rig.pump();
        rig.until(
            "the paused deck shows the intro",
            Rig::DEADLINE,
            Rig::frame,
            |rig| rig.scalar("deck.playback.position_secs@deck=a") < consts::TRACK_FIRST_BEAT,
        );
        assert!(rig.flag("deck.playback.synced@deck=a"));
        let paused = peak(rig, consts::PULSE);
        assert!(
            paused < consts::LEVEL_TOLERANCE,
            "the pause silences deck A, peak {paused}"
        );
        rig.send("deck-a/play", ControlAction::Activate);
        rig.pump();
        let played = peak(rig, consts::PLAY_ANSWER);
        assert!(
            played > consts::LEVEL_TOLERANCE,
            "Play sounds deck A, peak {played}"
        );
    })
    .await;
    rig.close().await;
    drop(served);
}

/// Pauses deck A for a moment.
fn tap_pause(rig: &mut Rig) {
    rig.send("deck-a/play", ControlAction::Activate);
    rig.pump();
    let _ = capture(rig, consts::PULSE);
    let paused = peak(rig, consts::PAUSE);
    assert!(
        paused < consts::LEVEL_TOLERANCE,
        "the pause silences deck A, peak {paused}"
    );
}

/// A Play taking back a tap pause sounds without waiting out a bar. Preparation
/// runs on wall time, so the answer renders at the output's own pace.
#[kithara::test(native, tokio, flash(false))]
async fn a_resume_on_a_synced_deck_sounds_at_once(constant_fifth_wav: String) {
    let Synced { rig, served } = synced_one(constant_fifth_wav, consts::TRACK_BPM).await;
    rig.call(|rig| {
        let _ = capture(rig, consts::SETTLE);
        tap_pause(rig);
        let pressed = rig.rendered_frames;
        rig.send("deck-a/play", ControlAction::Activate);
        rig.pump();
        let rate = f64::from(mock::host(rig).sample_rate().get());
        let block = Duration::from_secs_f64(AsPrimitive::<f64>::as_(consts::BLOCK_FRAMES) / rate);
        let Rendered { start, pcm } = capture_paced(rig, consts::RESUME_ANSWER, block);
        let start = u64::try_from(start).expect("the session axis starts at zero");
        let answer = pcm
            .chunks_exact(usize::from(consts::CHANNELS))
            .position(|frame| {
                frame
                    .iter()
                    .any(|sample| sample.abs() > consts::LEVEL_TOLERANCE)
            })
            .map(|frame| {
                let frame: u64 = frame.as_();
                Duration::from_secs_f64(AsPrimitive::<f64>::as_(start + frame - pressed) / rate)
            });
        assert!(
            answer.is_some_and(|answer| answer < consts::RESUME_ANSWER),
            "deck A sounds within {:?} of Play, it took {answer:?}",
            consts::RESUME_ANSWER
        );
    })
    .await;
    rig.close().await;
    drop(served);
}

/// A resumed deck keeps every click on a host beat and the accents on the bar.
#[kithara::test(native, tokio, flash(false))]
async fn a_resume_on_a_synced_deck_lands_on_the_host_bar(rhythm_a_wav: String) {
    let Synced { rig, served } = synced_one(rhythm_a_wav, 124.0).await;
    rig.call(|rig| {
        let _ = capture(rig, consts::SETTLE);
        tap_pause(rig);
        rig.send("deck-a/play", ControlAction::Activate);
        rig.pump();
        let _ = capture(rig, consts::RESUME_WINDOW);
        assert_on_host_beats(rig, 124.0, consts::INCOMING_BEATS);
    })
    .await;
    rig.close().await;
    drop(served);
}

#[kithara::test(native, tokio, flash(false))]
async fn a_synced_deck_crossfades_into_a_next_track_analysed_before_it_loads(
    rhythm_a_wav: String,
    rhythm_b_wav: String,
) {
    crossfade_into_the_next_track(rhythm_a_wav, rhythm_b_wav, true).await;
}

#[kithara::test(native, tokio, flash(false))]
async fn a_synced_deck_crossfades_into_a_next_track_analysed_after_it_loads(
    rhythm_a_wav: String,
    rhythm_b_wav: String,
) {
    crossfade_into_the_next_track(rhythm_a_wav, rhythm_b_wav, false).await;
}

#[kithara::test(native, tokio, flash(false))]
async fn a_synced_deck_shows_the_played_speed_and_keeps_it_when_sync_ends(rhythm_a_wav: String) {
    let Synced { rig, served } = synced(vec![rhythm_a_wav], 126.0, |requests, tracks| {
        task::spawn(fixtures::serve_grid(
            requests,
            tracks[0],
            grid_with_an_early_last_beat(),
        ))
    })
    .await;
    rig.call(|rig| {
        let _ = capture(rig, consts::SETTLE);
        rig.frame();
        assert_eq!(
            (
                rig.text("deck.playback.tempo@deck=a"),
                rig.text("deck.playback.bpm@deck=a"),
            ),
            (Some("+5.0%".to_owned()), Some("126.0".to_owned())),
            "the deck shows the speed the host plays it at"
        );

        rig.send("deck-a/sync", ControlAction::Activate);
        rig.pump();
        rig.until("deck A turns SYNC off", Rig::DEADLINE, Rig::frame, |rig| {
            !rig.flag("deck.playback.synced@deck=a")
        });
        let _ = capture(rig, consts::SETTLE);
        rig.frame();
        assert_eq!(
            rig.text("deck.playback.tempo@deck=a").as_deref(),
            Some("+5.0%"),
            "the track keeps the speed the host played it at"
        );

        rig.send("deck-a/tempo", ControlAction::StepScalar(1.0));
        rig.pump();
        rig.until("the tempo fader steps", Rig::DEADLINE, Rig::frame, |rig| {
            rig.text("deck.playback.tempo@deck=a").as_deref() != Some("+5.0%")
        });
        assert_eq!(
            rig.text("deck.playback.tempo@deck=a").as_deref(),
            Some("+6.5%"),
            "the fader steps on from the played speed"
        );
    })
    .await;
    rig.close().await;
    drop(served);
}

#[kithara::test(native, tokio, flash(false))]
async fn master_bpm_steps_add_up_whenever_the_engine_applies_them() {
    let rig = OffThread::spawn("engine", || Ok::<_, Infallible>(Rig::playback()))
        .await
        .expect("rig fixture is infallible");
    rig.call(|rig| {
        rig.frame();
        assert_eq!(
            rig.text("clock.bpm").as_deref(),
            Some("120.00"),
            "the bar draws the host tempo"
        );

        for pending in [2, 1] {
            for _ in 0..pending {
                rig.send("bar/tempo", ControlAction::StepScalar(1.0));
            }
            while let Ok(envelope) = rig.commands.try_recv() {
                rig.engine.apply(envelope);
            }
            rig.engine.publish();
            rig.frame();
        }
        rig.until(
            "the host confirms three MASTER BPM steps",
            Rig::DEADLINE,
            Rig::frame,
            |rig| (f64::from(mock::host(rig).tempo()) - 120.3).abs() < 1e-9,
        );
        assert_eq!(
            rig.text("clock.bpm").as_deref(),
            Some("120.30"),
            "the bar draws the confirmed tempo"
        );
    })
    .await;
    rig.close().await;
}
