#![cfg(not(target_arch = "wasm32"))]

use std::{collections::BTreeMap, num::NonZeroU32};

use kithara_command::When;
use kithara_platform::sync::Arc;
use kithara_signal::{AudioSpec, OutputContext, SegmentId, SessionEpoch, SessionFrame};
use kithara_test_fixtures::{integration_fixtures::deadline_tracks, signal::peak};
use kithara_test_utils::{
    kithara,
    test::usdt::{self, ProbeEvent},
};
use kithara_warp::RenderContext;

use super::{
    DeckMixerConfig, DeckPart, Fade, PcmConsumer, PlayerResource, Slot, StreamShape, TestEnds,
    TestMixer, chunk, mixer_with_shape, pools, send,
};
use crate::worker::{PcmPacket, packet_tests::PacketRing};

mod consts {
    pub(in crate::rt) const BLOCK_FRAMES: [u32; 4] = [128, 256, 512, 1_024];
    /// Callbacks one census window covers. A mix that walked its track list per
    /// frame would fire the probe `block_frames` times per track per callback,
    /// and this window keeps even that history inside `usdt::MAX_EVENTS`, so
    /// the walk count is what fails there, not the recorder.
    pub(in crate::rt) const CENSUS_BLOCKS: usize = 32;
    pub(in crate::rt) const CHANNELS: u16 = 2;
    /// Callbacks a cell renders after warmup, read as
    /// `MEASURED_BLOCKS / CENSUS_BLOCKS` windows. Every one of them is checked
    /// for the exact sum of all its tracks, so the length is the PCM evidence;
    /// the walk count is already exact inside a single window.
    pub(in crate::rt) const MEASURED_BLOCKS: usize = 4_096;
    pub(in crate::rt) const SAMPLE_RATE: u32 = 48_000;
    pub(in crate::rt) const TRACK_COUNTS: [usize; 3] = [1, 2, 4];
    pub(in crate::rt) const TRACK_SECONDS: f64 = 300.0;
    pub(in crate::rt) const WARMUP_BLOCKS: usize = 512;
}

struct DeadlineMixer {
    mixer: TestMixer,
    packets: Vec<(PacketRing, &'static [u8])>,
    shape: StreamShape,
    frame: u64,
}

fn non_zero(value: u32, label: &str) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap_or_else(|| panic!("{label} must be non-zero"))
}

fn spec() -> AudioSpec {
    AudioSpec::new(
        consts::CHANNELS,
        non_zero(consts::SAMPLE_RATE, "sample rate"),
    )
}

fn processor(block_frames: u32) -> (DeadlineMixer, TestEnds) {
    let shape = StreamShape::new(non_zero(block_frames, "block frames"), spec().sample_rate);
    let (mixer, ends) = mixer_with_shape(DeckMixerConfig::default(), 64, shape);
    (
        DeadlineMixer {
            mixer,
            packets: Vec::new(),
            shape,
            frame: 0,
        },
        ends,
    )
}

fn load_tracks(
    processor: &mut DeadlineMixer,
    control: &mut TestEnds,
    count: usize,
    deadline_tracks: [&'static [u8]; 4],
    (out_l, out_r): (&mut [f32], &mut [f32]),
) -> f32 {
    let mut expected_sample = 0.0;
    for (index, input) in deadline_tracks.into_iter().take(count).enumerate() {
        let value = f32::from(u16::try_from(index + 1).expect("track index fits u16")) * 0.02;
        expected_sample += value;
        let mut packets = PacketRing::new(
            spec(),
            kithara_platform::time::Duration::from_secs_f64(consts::TRACK_SECONDS),
            2,
        );
        let pcm = PlayerResource::new(
            PcmConsumer::new(packets.receiver.take().expect("packet receiver")),
            Arc::from(format!("no-sync-deadline-track-{index}")),
            &pools(),
        )
        .expect("resource fits pool budget");
        let slot = Slot::new(u16::try_from(index).expect("slot fits u16"));
        send(
            control,
            When::Next,
            vec![
                DeckPart::Attach {
                    slot,
                    pcm: Box::new(pcm),
                    segment: SegmentId::FIRST,
                },
                DeckPart::Start {
                    slot,
                    fade: Fade::Declick,
                },
            ],
        );
        processor.packets.push((packets, input));
    }
    render_block(processor, out_l, out_r);
    expected_sample
}

fn render_block(processor: &mut DeadlineMixer, out_l: &mut [f32], out_r: &mut [f32]) {
    let frames = out_l.len();
    for (packets, input) in &mut processor.packets {
        while packets.returned().is_some() {}
        let samples: Vec<_> = input
            .chunks_exact(4)
            .cycle()
            .take(frames * 2)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("sample bytes")))
            .collect();
        packets.push(PcmPacket::Chunk(Box::new(chunk(
            spec(),
            SegmentId::FIRST,
            processor.frame,
            processor.frame,
            &samples,
        ))));
    }
    out_l.fill(0.0);
    out_r.fill(0.0);
    let inputs: [&[f32]; 0] = [];
    let mut outputs = [out_l, out_r];
    let mut buffers = firewheel::node::ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    let start =
        SessionFrame::new(i64::try_from(processor.frame).expect("frame fits session clock"));
    processor.frame += u64::try_from(frames).expect("frames fit clock");
    let end = SessionFrame::new(i64::try_from(processor.frame).expect("frame fits session clock"));
    let context = RenderContext::new_linear(
        OutputContext::new(
            start..end,
            processor.shape.sample_rate,
            SessionEpoch::new(0),
            None,
        )
        .expect("output range"),
        None,
    )
    .expect("render context");
    processor.mixer.inbox.0.drain();
    let level = processor
        .mixer
        .inbox
        .0
        .scope(processor.mixer.mixer.scope)
        .expect("live deck scope");
    processor
        .mixer
        .mixer
        .render_block(Some(level), Some(&context), start, &mut buffers, frames);
}

fn assert_all_tracks_contributed(
    samples: &[f32],
    expected_sample: f32,
    block_frames: u32,
    tracks: usize,
) {
    assert!(
        samples
            .iter()
            .all(|sample| (*sample - expected_sample).abs() <= 1.0e-5),
        "deadline cell must render the exact sum of all {tracks} track(s) at {block_frames} frames: expected {expected_sample}, observed peak {}",
        peak(samples),
    );
}

/// How many times the mix walked each track, keyed by the track it walked.
fn walks_per_track(recorded: &[ProbeEvent]) -> BTreeMap<u64, usize> {
    let mut walks = BTreeMap::new();
    for event in recorded.iter().filter(|event| event.probe == "render") {
        let track = event
            .field("track_id")
            .expect("the render probe carries the track it walked");
        *walks.entry(track).or_insert(0_usize) += 1;
    }
    walks
}

fn assert_one_walk_per_track_per_block(recorded: &[ProbeEvent], block_frames: u32, tracks: usize) {
    let walks = walks_per_track(recorded);
    assert_eq!(
        walks.len(),
        tracks,
        "a census window at {block_frames} frames must walk every one of the \
         {tracks} playing track(s), observed {walks:?}"
    );
    assert!(
        walks.values().all(|count| *count == consts::CENSUS_BLOCKS),
        "the mix must walk each of {tracks} track(s) once per callback; \
         {} callbacks at {block_frames} frames walked {walks:?}",
        consts::CENSUS_BLOCKS,
    );
}

fn assert_each_walk_covers_the_block(recorded: &[ProbeEvent], block_frames: u32, tracks: usize) {
    let sliced = recorded
        .iter()
        .filter(|event| event.probe == "render")
        .find(|event| {
            event.field("range_start") != Some(0)
                || event.field("range_end") != Some(u64::from(block_frames))
        });
    assert!(
        sliced.is_none(),
        "a mix walk must cover the whole callback of {block_frames} frames with \
         {tracks} track(s), not a slice of it: {sliced:?}"
    );
}

fn census(block_frames: u32, tracks: usize, deadline_tracks: [&'static [u8]; 4]) {
    let (mut processor, mut control) = processor(block_frames);
    let frames = usize::try_from(block_frames).expect("block frames fit usize");
    let mut out_l = vec![0.0_f32; frames];
    let mut out_r = vec![0.0_f32; frames];
    let expected_sample = load_tracks(
        &mut processor,
        &mut control,
        tracks,
        deadline_tracks,
        (&mut out_l, &mut out_r),
    );
    assert_eq!(
        processor.mixer.mixer.deck.tracks.iter_mut().count(),
        tracks,
        "deadline cell must load exactly {tracks} active track(s)"
    );

    let metrics_before = processor.mixer.deck.metrics.snapshot();

    for _ in 0..consts::WARMUP_BLOCKS {
        render_block(&mut processor, &mut out_l, &mut out_r);
    }
    assert!(
        peak(&out_l).max(peak(&out_r)) > 0.0,
        "deadline cell must reach audible PCM before the census ({block_frames} frames, {tracks} track(s))"
    );
    assert_all_tracks_contributed(&out_l, expected_sample, block_frames, tracks);
    assert_all_tracks_contributed(&out_r, expected_sample, block_frames, tracks);

    for _ in 0..consts::MEASURED_BLOCKS / consts::CENSUS_BLOCKS {
        let trace = usdt::scope();
        for _ in 0..consts::CENSUS_BLOCKS {
            render_block(&mut processor, &mut out_l, &mut out_r);
            assert_all_tracks_contributed(&out_l, expected_sample, block_frames, tracks);
            assert_all_tracks_contributed(&out_r, expected_sample, block_frames, tracks);
        }
        let recorded = trace.events();
        drop(trace);
        assert_one_walk_per_track_per_block(&recorded, block_frames, tracks);
        assert_each_walk_covers_the_block(&recorded, block_frames, tracks);
    }

    let metrics_after = processor.mixer.deck.metrics.snapshot();
    assert_eq!(
        metrics_after.underruns(),
        metrics_before.underruns(),
        "no-SYNC rendering must have zero underrun delta ({block_frames} frames, {tracks} track(s))"
    );
    assert_eq!(
        metrics_after.decode_errors(),
        metrics_before.decode_errors(),
        "no-SYNC rendering must have zero decode-error delta ({block_frames} frames, {tracks} track(s))"
    );
    assert_eq!(
        processor.mixer.mixer.deck.tracks.iter_mut().count(),
        tracks,
        "deadline cell must finish with exactly {tracks} active track(s)"
    );
    assert!(
        peak(&out_l).max(peak(&out_r)) > 0.0,
        "census callbacks must finish on audible PCM, not silence"
    );
}

/// Mixing a track costs the same however many tracks play.
///
/// The hot path scans the active tracks again inside its per-track loop, so a
/// stray per-frame step there turns the mix quadratic and the audio deadline
/// stops holding as the queue fills. The `render` probe fires on every walk the
/// mix makes over a track, so a callback's firings are that work itself: one
/// per playing track, each covering the whole block. A mix that walks per frame
/// multiplies both by the block size. Counting walks asks no clock, so the
/// verdict holds whatever else the runner is doing. Absolute block cost belongs
/// to the `rt_block_budget` bench, which times this processor and asks the
/// clock for no verdict.
#[kithara::test(native, serial)]
fn mixing_a_track_costs_the_same_however_many_tracks_play(deadline_tracks: [&'static [u8]; 4]) {
    for block_frames in consts::BLOCK_FRAMES {
        for tracks in consts::TRACK_COUNTS {
            census(block_frames, tracks, deadline_tracks);
        }
    }
}
