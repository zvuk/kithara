//! The audio thread reports trouble through lock-free counters, not `tracing`.
//!
//! Each test drives one failure branch of `process()` and asserts the matching counter moved, so a
//! change that reintroduces a log — or drops the signal altogether — fails here.
#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use firewheel::node::ProcBuffers;
use kithara::{
    events::TrackId,
    platform::{sync::Arc, time::Duration},
    play::{
        Resource, SharedEq,
        bridge::{PlayerCmd, RtMetricsSnapshot, SlotControl, slot_channels},
        rt::{PlayerNodeProcessor, StreamShape, track::PlayerResource},
    },
    signal::AudioSpec,
};
use kithara_integration_tests::audio_mock::{
    Fault, FaultyPcmReader, SeekSplitReader, TestPcmReader,
};
use ringbuf::traits::Producer;

use crate::bufpool_ext::pools;

const SAMPLE_RATE: u32 = 48_000;
const BLOCK_FRAMES: u32 = 128;

fn block_len() -> usize {
    usize::try_from(BLOCK_FRAMES).expect("block frames fit usize")
}

fn spec() -> AudioSpec {
    AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("non-zero rate"))
}

fn processor() -> (PlayerNodeProcessor, SlotControl) {
    let (inputs, control) = slot_channels(SharedEq::new(0));
    let shape = StreamShape::new(
        NonZeroU32::new(BLOCK_FRAMES).expect("non-zero block"),
        NonZeroU32::new(SAMPLE_RATE).expect("non-zero rate"),
    );
    (PlayerNodeProcessor::new(inputs, shape, &pools()), control)
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
    Box::new(
        PlayerResource::new(resource, Arc::from(src), &pools())
            .expect("player resource fits the test pool budget"),
    )
}

fn load(control: &mut SlotControl, resource: Box<PlayerResource>) -> TrackId {
    let item_id = TrackId::allocate();
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack { resource, item_id })
        .ok();
    item_id
}

fn render_loaded_blocks(
    resource: Box<PlayerResource>,
    blocks: usize,
) -> (PlayerNodeProcessor, Vec<f32>) {
    let (mut processor, mut control) = processor();
    let item_id = load(&mut control, resource);
    control.cmd_tx.try_push(PlayerCmd::SetPaused(false)).ok();
    processor.drain_commands();

    if let Some(track) = processor.track_mut(item_id) {
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

fn render_loaded(resource: Box<PlayerResource>) -> PlayerNodeProcessor {
    render_loaded_blocks(resource, 1).0
}

fn metrics(processor: &PlayerNodeProcessor) -> RtMetricsSnapshot {
    processor.playback().metrics().snapshot()
}

#[kithara::test]
fn decode_error_is_counted_not_logged() {
    let processor = render_loaded(faulty_track("broken.mp3", Fault::DecodeError));

    assert!(
        metrics(&processor).decode_errors() > 0,
        "a decode error inside process() must land in the counters"
    );
}

#[kithara::test]
fn source_with_nothing_ready_renders_silence_and_counts_an_underrun() {
    let (processor, rendered) = render_loaded_blocks(faulty_track("stalled.mp3", Fault::Stall), 4);

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
    let processor = render_loaded(healthy_track("ok.mp3"));

    assert_eq!(metrics(&processor), RtMetricsSnapshot::default());
}

#[kithara::test]
fn a_seek_on_the_audio_thread_only_syncs_never_blocks() {
    let (reader, counts) = SeekSplitReader::new(spec());
    let (mut processor, mut control) = processor();
    let item_id = load(
        &mut control,
        boxed(Resource::from_reader(reader, None), "split.mp3"),
    );
    processor.drain_commands();

    if let Some(track) = processor.track_mut(item_id) {
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
        (processor.track(item_id).expect("track loaded").position() - 30.0).abs() < 0.001,
        "the media clock still re-bases on the new position"
    );
}

#[kithara::test]
fn the_slot_begins_seeks_for_the_tracks_it_shipped() {
    let (reader, counts) = SeekSplitReader::new(spec());
    let resource = boxed(Resource::from_reader(reader, None), "split.mp3");
    let handle = resource.seek_handle().expect("reader splits its seek");
    let (_, mut control) = processor();
    let item_id = TrackId::allocate();

    control.bind_seek(item_id, Arc::clone(&handle));
    control.begin_seek(Duration::from_secs(30));
    assert_eq!(counts.begins(), 1);
    assert_eq!(counts.blocking_seeks(), 0);

    control.unbind_seek(item_id, &handle);
    control.begin_seek(Duration::from_secs(45));
    assert_eq!(
        counts.begins(),
        1,
        "an unloaded track must not be seeked any more"
    );
}

#[kithara::test]
fn unloading_one_seek_binding_preserves_other_identity() {
    let (first_reader, first_counts) = SeekSplitReader::new(spec());
    let first = boxed(Resource::from_reader(first_reader, None), "same.mp3");
    let first_handle = first.seek_handle().expect("reader splits its seek");
    let first_id = TrackId::allocate();

    let (second_reader, second_counts) = SeekSplitReader::new(spec());
    let second = boxed(Resource::from_reader(second_reader, None), "same.mp3");
    let second_handle = second.seek_handle().expect("reader splits its seek");
    let second_id = TrackId::allocate();

    let (_, mut control) = processor();
    control.bind_seek(first_id, Arc::clone(&first_handle));
    control.bind_seek(second_id, Arc::clone(&second_handle));
    control.unbind_seek(first_id, &first_handle);
    control.begin_seek(Duration::from_secs(30));

    assert_eq!(first_counts.begins(), 0, "the unloaded item stays detached");
    assert_eq!(
        second_counts.begins(),
        1,
        "the other queue item keeps its seek path despite sharing the URL"
    );

    let (replacement_reader, replacement_counts) = SeekSplitReader::new(spec());
    let replacement = boxed(Resource::from_reader(replacement_reader, None), "same.mp3");
    let replacement_handle = replacement.seek_handle().expect("reader splits its seek");
    control.bind_seek(second_id, replacement_handle);
    control.unbind_seek(second_id, &second_handle);
    control.begin_seek(Duration::from_secs(45));

    assert_eq!(
        second_counts.begins(),
        1,
        "the retired resource generation stays detached"
    );
    assert_eq!(
        replacement_counts.begins(),
        1,
        "retiring the old generation must keep its replacement bound"
    );
}

#[kithara::test]
fn evicting_an_audible_track_is_counted() {
    let (mut processor, mut control) = processor();

    for idx in 0..PlayerNodeProcessor::MAX_TRACKS {
        let src = format!("track-{idx}.mp3");
        let item_id = load(&mut control, healthy_track(&src));
        processor.drain_commands();
        if let Some(track) = processor.track_mut(item_id) {
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
    let item_id = load(&mut control, healthy_track("ok.mp3"));
    processor.drain_commands();
    if let Some(track) = processor.track_mut(item_id) {
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
