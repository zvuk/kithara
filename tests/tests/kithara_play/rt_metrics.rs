//! The audio thread reports trouble through lock-free counters, not `tracing`.
//!
//! Each test drives one failure branch of `process()` and asserts the matching
//! counter moved, so a change that reintroduces a log — or drops the signal
//! altogether — fails here.

#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use firewheel::node::ProcBuffers;
use kithara::{
    bufpool::PcmPool,
    decode::PcmSpec,
    platform::{sync::Arc, time::Duration},
    play::{
        Resource, SharedEq,
        bridge::{PlayerCmd, RtMetricsSnapshot, SlotControl, slot_channels},
        rt::{PlayerNodeProcessor, StreamShape, track::PlayerResource},
    },
};
use kithara_integration_tests::audio_mock::{
    Fault, FaultyPcmReader, SeekSplitReader, TestPcmReader,
};
use ringbuf::traits::Producer;

const SAMPLE_RATE: u32 = 48_000;
const BLOCK_FRAMES: u32 = 128;

fn block_len() -> usize {
    usize::try_from(BLOCK_FRAMES).expect("block frames fit usize")
}

fn spec() -> PcmSpec {
    PcmSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("non-zero rate"))
}

fn processor() -> (PlayerNodeProcessor, SlotControl) {
    let (inputs, control) = slot_channels(SharedEq::new(0));
    let shape = StreamShape {
        sample_rate: NonZeroU32::new(SAMPLE_RATE).expect("non-zero rate"),
        max_block_frames: NonZeroU32::new(BLOCK_FRAMES).expect("non-zero block"),
    };
    (
        PlayerNodeProcessor::new(inputs, shape, &PcmPool::default()),
        control,
    )
}

fn faulty_track(src: &str, fault: Fault) -> Box<PlayerResource> {
    boxed(
        Resource::from_reader(FaultyPcmReader::new(spec(), fault), None),
        src,
    )
}

fn healthy_track(src: &str) -> Box<PlayerResource> {
    boxed(
        Resource::from_reader(TestPcmReader::new(spec(), 60.0), None),
        src,
    )
}

fn boxed(resource: Resource, src: &str) -> Box<PlayerResource> {
    Box::new(PlayerResource::new(
        resource,
        Arc::from(src),
        &PcmPool::default(),
    ))
}

fn load(control: &mut SlotControl, resource: Box<PlayerResource>) {
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource,
            item_id: None,
        })
        .ok();
}

fn render_loaded_blocks(
    src: &str,
    resource: Box<PlayerResource>,
    blocks: usize,
) -> (PlayerNodeProcessor, Vec<f32>) {
    let (mut processor, mut control) = processor();
    load(&mut control, resource);
    control.cmd_tx.try_push(PlayerCmd::SetPaused(false)).ok();
    processor.drain_commands();

    if let Some(track) = processor.track_mut(&Arc::from(src)) {
        track.play();
    }

    let mut out_l = vec![0.0f32; block_len()];
    for _ in 0..blocks {
        let mut out_r = vec![0.0f32; block_len()];
        let inputs: [&[f32]; 0] = [];
        let mut outputs = [&mut out_l[..], &mut out_r[..]];
        let mut buffers = ProcBuffers {
            inputs: &inputs,
            outputs: &mut outputs,
        };
        let _ = processor.render_audio(&mut buffers, block_len(), true);
    }

    (processor, out_l)
}

fn render_loaded(src: &str, resource: Box<PlayerResource>) -> PlayerNodeProcessor {
    render_loaded_blocks(src, resource, 1).0
}

fn metrics(processor: &PlayerNodeProcessor) -> RtMetricsSnapshot {
    processor.playback().metrics().snapshot()
}

#[kithara::test]
fn decode_error_is_counted_not_logged() {
    let processor = render_loaded("broken.mp3", faulty_track("broken.mp3", Fault::DecodeError));

    assert!(
        metrics(&processor).decode_errors() > 0,
        "a decode error inside process() must land in the counters"
    );
}

#[kithara::test]
fn source_with_nothing_ready_renders_silence_and_counts_an_underrun() {
    let (processor, rendered) =
        render_loaded_blocks("stalled.mp3", faulty_track("stalled.mp3", Fault::Stall), 4);

    assert!(
        metrics(&processor).underruns() > 0,
        "a zero-filled block short of EOF is an underrun"
    );
    let peak = rendered.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
    assert!(
        peak == 0.0,
        "an underrun must render silence, not stale scratch (peak {peak})"
    );
}

#[kithara::test]
fn a_healthy_track_reports_no_trouble() {
    let processor = render_loaded("ok.mp3", healthy_track("ok.mp3"));

    assert_eq!(metrics(&processor), RtMetricsSnapshot::default());
}

#[kithara::test]
fn a_seek_on_the_audio_thread_only_syncs_never_blocks() {
    let (reader, counts) = SeekSplitReader::new(spec());
    let (mut processor, mut control) = processor();
    load(
        &mut control,
        boxed(Resource::from_reader(reader, None), "split.mp3"),
    );
    processor.drain_commands();

    let src: Arc<str> = Arc::from("split.mp3");
    if let Some(track) = processor.track_mut(&src) {
        track.seek(30.0);
    }

    assert_eq!(
        counts.blocking_seeks(),
        0,
        "the audio thread must not reach the blocking seek"
    );
    assert_eq!(
        counts.syncs(),
        1,
        "it adopts the target that begin published"
    );
    assert_eq!(
        counts.begins(),
        0,
        "beginning belongs to the control thread, not to this call"
    );
    assert!(
        (processor.track(&src).expect("track loaded").position() - 30.0).abs() < 0.001,
        "the media clock still re-bases on the new position"
    );
}

#[kithara::test]
fn the_slot_begins_seeks_for_the_tracks_it_shipped() {
    let (reader, counts) = SeekSplitReader::new(spec());
    let resource = boxed(Resource::from_reader(reader, None), "split.mp3");
    let handle = resource.seek_handle().expect("reader splits its seek");
    let (_, mut control) = processor();

    control.bind_seek(Arc::from("split.mp3"), handle);
    control.begin_seek(Duration::from_secs(30));
    assert_eq!(counts.begins(), 1);
    assert_eq!(counts.blocking_seeks(), 0);

    control.unbind_seek("split.mp3");
    control.begin_seek(Duration::from_secs(45));
    assert_eq!(
        counts.begins(),
        1,
        "an unloaded track must not be seeked any more"
    );
}

#[kithara::test]
fn evicting_an_audible_track_is_counted() {
    let (mut processor, mut control) = processor();

    for idx in 0..PlayerNodeProcessor::MAX_TRACKS {
        let src = format!("track-{idx}.mp3");
        load(&mut control, healthy_track(&src));
        processor.drain_commands();
        if let Some(track) = processor.track_mut(&Arc::from(src.as_str())) {
            track.play();
        }
    }

    load(&mut control, healthy_track("newcomer.mp3"));
    processor.drain_commands();

    assert!(
        metrics(&processor).evicted_playing() > 0,
        "dropping an audible track to make room is a real defect and must stay visible"
    );
}

#[kithara::test]
fn a_block_larger_than_declared_is_clamped_not_grown() {
    let (mut processor, mut control) = processor();
    load(&mut control, healthy_track("ok.mp3"));
    processor.drain_commands();
    if let Some(track) = processor.track_mut(&Arc::from("ok.mp3")) {
        track.play();
    }

    let oversized = block_len() * 2;
    let mut out_l = vec![f32::NAN; oversized];
    let mut out_r = vec![f32::NAN; oversized];

    let inputs: [&[f32]; 0] = [];

    let mut outputs = [&mut out_l[..], &mut out_r[..]];

    let mut buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };

    let (rendered, _) = processor.render_audio(&mut buffers, oversized, true);

    assert!(rendered, "the declared part of the block still renders");
    assert!(
        out_l[..block_len()].iter().all(|s| s.is_finite()),
        "frames up to max_block_frames are written"
    );
    assert!(
        out_l[block_len()..].iter().all(|s| *s == 0.0),
        "frames beyond the declared block are silence, since the host is told the \
         whole block is valid"
    );
}
