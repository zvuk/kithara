use super::*;
#[cfg(feature = "stretch-signalsmith")]
use crate::region::{GridSegment, RegionPlan};

#[cfg(feature = "stretch-signalsmith")]
fn sized_chunk(value: f32, frames: usize, frame_offset: u64, spec: PcmSpec) -> PcmChunk {
    let mut meta = PcmMeta::default();
    meta.spec = spec;
    meta.frames = u32::try_from(frames).expect("test frame count fits u32");
    meta.frame_offset = frame_offset;
    meta.end_timestamp = duration_for_frames(
        spec.sample_rate.get(),
        frame_offset + u64::try_from(frames).expect("test frame count fits u64"),
    );
    let samples = frames * usize::from(spec.channels);
    PcmChunk::new(meta, PcmPool::default().attach(vec![value; samples]))
}

#[cfg(feature = "stretch-signalsmith")]
fn tempo_presentation(
    controls: &Arc<StretchControls>,
    initial_spec: PcmSpec,
    epoch: u64,
) -> (
    Presentation,
    crate::runtime::Inlet<Fetch<PresentedPcm>>,
    PresentationFrontier,
) {
    let pool = PcmPool::new(128, 200_000);
    let chain = create_presentation_chain(initial_spec, Some(controls), &pool, Vec::new());
    let (output, input) = connect_strict(PRESENTATION_RING_BLOCKS, None);
    let (publisher, frontier) = presentation_cell(epoch);
    (
        Presentation::new(4, chain, pool, initial_spec, output, publisher, epoch),
        input,
        frontier,
    )
}

#[cfg(feature = "stretch-signalsmith")]
fn serviced_step(presentation: &mut Presentation) -> DecodeResult<PresentResult> {
    presentation.service_off_rt();
    let mut retired = None;
    let result = presentation.step(|chunk| {
        debug_assert!(retired.is_none());
        retired = Some(chunk);
    });
    drop(retired);
    result
}

#[kithara::test]
fn strict_output_never_parks_a_third_block() {
    let (mut presentation, mut input) = identity_presentation(Vec::new());
    admit(
        &mut presentation,
        Fetch::data(chunk(1.0, 0), 0),
        "first raw chunk fits",
    );
    admit(
        &mut presentation,
        Fetch::data(chunk(2.0, 512), 0),
        "second raw chunk fits",
    );

    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(_))
    ));
    assert_eq!(step(&mut presentation), PresentResult::Backpressured);
    let current = input.try_pop().expect("consumer holds one block");
    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(_))
    ));
    assert_eq!(data(&mut presentation, current).samples[0], 1.0);
    assert_eq!(
        data(
            &mut presentation,
            input.try_pop().expect("ring holds one block"),
        )
        .samples[0],
        2.0
    );
}

#[kithara::test]
fn empty_terminal_opens_preload_after_marker_commit() {
    let (mut presentation, mut input) = identity_presentation(Vec::new());
    presentation.finish_eof(0);

    assert!(!presentation.preload_ready(3));
    assert_eq!(step(&mut presentation), PresentResult::Terminal);
    assert!(input.try_pop().is_some());
    assert!(presentation.preload_ready(3));
}

#[kithara::test]
fn preload_requires_raw_target_and_one_final_commit() {
    let (mut presentation, mut input) = identity_presentation(Vec::new());
    for block in 0_u16..3 {
        admit(
            &mut presentation,
            Fetch::data(chunk(f32::from(block), u64::from(block) * 512), 0),
            "preload raw block fits",
        );
    }

    assert!(!presentation.preload_ready(3));
    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(_))
    ));
    assert!(presentation.preload_ready(3));
    assert!(input.try_pop().is_some());
}

#[kithara::test]
fn failure_drains_admitted_pcm_then_resets_without_eof_tail() {
    let resets = Arc::new(AtomicUsize::new(0));
    let (mut presentation, mut input) = identity_presentation(vec![Box::new(StampEffect {
        resets: Some(Arc::clone(&resets)),
        seen_frames: Arc::new(AtomicUsize::new(0)),
        value: 6.0,
    })]);
    admit(
        &mut presentation,
        Fetch::data(chunk(1.0, 0), 0),
        "first raw chunk fits",
    );
    admit(
        &mut presentation,
        Fetch::data(chunk(2.0, 512), 0),
        "second raw chunk fits",
    );
    presentation.finish_failed(0);

    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(_))
    ));
    assert_eq!(
        data(&mut presentation, input.try_pop().expect("first prior PCM"),).samples[0],
        6.0
    );
    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(_))
    ));
    assert_eq!(
        data(
            &mut presentation,
            input.try_pop().expect("second prior PCM"),
        )
        .samples[0],
        6.0
    );
    assert_eq!(step(&mut presentation), PresentResult::Terminal);
    assert!(matches!(input.try_pop(), Some(Fetch::Failure { epoch: 0 })));
    assert_eq!(resets.load(Ordering::Acquire), 1);
}

#[kithara::test]
fn epoch_reset_retires_raw_and_resets_chain_once() {
    let resets = Arc::new(AtomicUsize::new(0));
    let (mut presentation, _input) = identity_presentation(vec![Box::new(StampEffect {
        resets: Some(Arc::clone(&resets)),
        seen_frames: Arc::new(AtomicUsize::new(0)),
        value: 1.0,
    })]);
    admit(
        &mut presentation,
        Fetch::data(chunk(1.0, 0), 0),
        "raw chunk fits",
    );
    let mut retired = 0;

    presentation
        .reset_epoch(3, |_| retired += 1)
        .expect("epoch reset succeeds");

    assert_eq!(retired, 1);
    assert_eq!(resets.load(Ordering::Acquire), 1);
    assert_eq!(presentation.epoch(), 3);
    assert_eq!(step(&mut presentation), PresentResult::Idle);
}

#[kithara::test]
fn stale_decoder_barrier_is_rejected_without_rewinding_epoch() {
    let (mut presentation, _input) = identity_presentation(Vec::new());
    let replacement = PresentationBarrier::DecoderReplaced {
        epoch: 9,
        spec: spec(48_000),
    };

    assert_eq!(presentation.admit_barrier(replacement), Err(replacement),);
    assert_eq!(presentation.epoch(), 0);
    assert_eq!(step(&mut presentation), PresentResult::Idle);
}

#[kithara::test]
fn empty_raw_chunk_fails_before_final_output() {
    let (mut presentation, mut input) = identity_presentation(Vec::new());
    let mut meta = PcmMeta::default();
    meta.spec.channels = 1;
    let empty = PcmChunk::new(meta, PcmPool::default().attach(Vec::new()));
    admit(
        &mut presentation,
        Fetch::data(empty, 0),
        "empty raw item still enters the typed presentation boundary",
    );
    let mut retired = 0;

    assert!(presentation.step(|_| retired += 1).is_err());
    assert_eq!(retired, 1);
    assert!(input.try_pop().is_none());
}

#[kithara::test]
fn same_epoch_decoder_barrier_resets_frontier_between_ordered_blocks() {
    let (output, mut input) = connect_strict(1, None);
    let (publisher, frontier) = presentation_cell(4);
    let mut presentation = Presentation::new(
        4,
        PresentationChain::identity(Vec::new()),
        PcmPool::default(),
        spec(44_100),
        output,
        publisher,
        4,
    );
    admit(
        &mut presentation,
        Fetch::data(chunk(1.0, 0), 4),
        "old decoder block fits",
    );
    presentation
        .admit_barrier(PresentationBarrier::DecoderReplaced {
            epoch: 4,
            spec: spec(48_000),
        })
        .expect("barrier fits");
    admit(
        &mut presentation,
        Fetch::data(chunk_with_spec(2.0, 512, spec(48_000)), 4),
        "new decoder block fits",
    );

    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(_))
    ));
    assert_eq!(
        data(&mut presentation, input.try_pop().expect("old output")).samples[0],
        1.0
    );
    presentation.service_off_rt();
    assert_eq!(
        frontier
            .snapshot()
            .expect("frontier is available")
            .source_frame(),
        512
    );
    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    let reset = frontier.snapshot().expect("reset frontier is available");
    assert_eq!(reset.generation(), 1);
    assert_eq!(reset.source_frame(), 512);
    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(_))
    ));
    assert_eq!(
        data(&mut presentation, input.try_pop().expect("new output")).samples[0],
        2.0
    );
}

#[kithara::test]
fn ordered_tempo_barrier_drains_held_source_before_reset() {
    let begins = Arc::new(AtomicUsize::new(0));
    let reconfigures = Arc::new(AtomicUsize::new(0));
    let tempo = SlowStage::new(Arc::new(AtomicUsize::new(0)), 0)
        .with_discontinuity(700, Arc::clone(&begins))
        .with_reconfigures(Arc::clone(&reconfigures));
    let (output, mut input) = connect_strict(1, None);
    let (publisher, frontier) = presentation_cell(4);
    let mut presentation = Presentation::new(
        4,
        PresentationChain {
            effects: Vec::new(),
            tempo: Some(Box::new(tempo)),
        },
        PcmPool::default(),
        spec(44_100),
        output,
        publisher,
        4,
    );
    admit(
        &mut presentation,
        Fetch::data(chunk(1.0, 0), 4),
        "first old raw block fits",
    );
    admit(
        &mut presentation,
        Fetch::data(chunk(2.0, 512), 4),
        "second old raw block fits",
    );
    presentation
        .admit_barrier(PresentationBarrier::DecoderReplaced {
            epoch: 4,
            spec: spec(48_000),
        })
        .expect("ordered barrier fits");

    for _ in 0..2 {
        assert_eq!(step(&mut presentation), PresentResult::Advanced);
        for _ in 0..2 {
            assert!(matches!(
                presentation.step(|_| {}),
                Ok(PresentResult::Produced(_))
            ));
            let _ = data(
                &mut presentation,
                input.try_pop().expect("old tempo output"),
            );
        }
    }
    assert_eq!(
        frontier
            .snapshot()
            .expect("frontier is available")
            .source_frame(),
        324
    );

    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    assert_eq!(begins.load(Ordering::Acquire), 1);
    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(PresentedBlock { frames: 512, .. }))
    ));
    let _ = data(
        &mut presentation,
        input.try_pop().expect("first discontinuity slice"),
    );
    assert_eq!(
        frontier
            .snapshot()
            .expect("frontier is available")
            .source_frame(),
        324
    );
    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(PresentedBlock { frames: 188, .. }))
    ));
    let _ = data(
        &mut presentation,
        input.try_pop().expect("final discontinuity slice"),
    );
    let final_old = frontier.snapshot().expect("frontier is available");
    assert_eq!(final_old.source_frame(), 324);
    assert_eq!(final_old.output_end(), 2_748);
    assert_eq!(reconfigures.load(Ordering::Acquire), 0);

    assert_eq!(step(&mut presentation), PresentResult::Backpressured);
    assert_eq!(reconfigures.load(Ordering::Acquire), 0);
    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    let reset = frontier.snapshot().expect("reset frontier is available");
    assert_eq!(reset.generation(), 1);
    assert_eq!(reset.source_frame(), 1_024);
    assert_eq!(reset.output_end(), 0);
    assert_eq!(reconfigures.load(Ordering::Acquire), 1);

    admit(
        &mut presentation,
        Fetch::data(chunk_with_spec(3.0, 1_024, spec(48_000)), 4),
        "replacement decoder block fits",
    );
    assert_eq!(step(&mut presentation), PresentResult::Advanced);
}

#[kithara::test]
fn control_boundary_without_decoder_barrier_drains_and_commits() {
    let requested = Arc::new(AtomicUsize::new(0));
    let begins = Arc::new(AtomicUsize::new(0));
    let reconfigures = Arc::new(AtomicUsize::new(0));
    let tempo = SlowStage::new(Arc::new(AtomicUsize::new(0)), 0)
        .with_control_boundary(Arc::clone(&requested))
        .with_discontinuity(700, Arc::clone(&begins))
        .with_reconfigures(Arc::clone(&reconfigures));
    let (output, mut input) = connect_strict(1, None);
    let (publisher, frontier) = presentation_cell(4);
    let mut presentation = Presentation::new(
        4,
        PresentationChain {
            effects: Vec::new(),
            tempo: Some(Box::new(tempo)),
        },
        PcmPool::default(),
        spec(44_100),
        output,
        publisher,
        4,
    );
    admit(
        &mut presentation,
        Fetch::data(chunk(1.0, 0), 4),
        "old raw block fits",
    );

    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    for _ in 0..2 {
        assert!(matches!(
            presentation.step(|_| {}),
            Ok(PresentResult::Produced(_))
        ));
        let _ = data(
            &mut presentation,
            input.try_pop().expect("old tempo output"),
        );
    }

    requested.store(1, Ordering::Release);
    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    assert_eq!(begins.load(Ordering::Acquire), 1);
    for expected_frames in [512, 188] {
        assert!(matches!(
            presentation.step(|_| {}),
            Ok(PresentResult::Produced(PresentedBlock { frames, .. }))
                if frames == expected_frames
        ));
        let _ = data(
            &mut presentation,
            input.try_pop().expect("control-boundary tail output"),
        );
    }
    assert_eq!(reconfigures.load(Ordering::Acquire), 0);

    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    assert_eq!(reconfigures.load(Ordering::Acquire), 1);
    assert_eq!(
        frontier
            .snapshot()
            .expect("frontier remains available")
            .generation(),
        0,
        "control changes do not reset the decoder generation"
    );

    admit(
        &mut presentation,
        Fetch::data(chunk(2.0, 512), 4),
        "next raw block fits after the control commit",
    );
    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(_))
    ));
}

#[cfg(feature = "stretch-signalsmith")]
#[kithara::test]
fn serviced_decoder_boundary_waits_for_partially_rendered_source() {
    const OLD_FRAMES: usize = 1_152;
    let old_spec = PcmSpec::new(
        2,
        NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
    );
    let replacement_spec = PcmSpec::new(
        2,
        NonZeroU32::new(48_000).expect("test sample rate is non-zero"),
    );
    let controls = StretchControls::new(1.0);
    let (mut presentation, mut input, _) = tempo_presentation(&controls, old_spec, 4);
    admit(
        &mut presentation,
        Fetch::data(sized_chunk(0.25, OLD_FRAMES, 0, old_spec), 4),
        "large old-spec chunk fits",
    );

    assert_eq!(
        serviced_step(&mut presentation).expect("old source admits"),
        PresentResult::Advanced
    );
    assert!(matches!(
        serviced_step(&mut presentation),
        Ok(PresentResult::Produced(PresentedBlock { frames: 512, .. }))
    ));
    let Fetch::Data { data: first, .. } = input.try_pop().expect("first old-spec output") else {
        panic!("expected old-spec PCM");
    };
    assert_eq!(first.point().generation(), 0);
    assert_eq!(first.chunk().spec(), old_spec);
    assert!(first.chunk().samples.iter().all(|sample| *sample == 0.25));
    let mut old_frames = first.chunk().frames();
    presentation.recycle_output(first.into());

    presentation
        .admit_barrier(PresentationBarrier::DecoderReplaced {
            epoch: 4,
            spec: replacement_spec,
        })
        .expect("replacement barrier fits behind admitted source");
    admit(
        &mut presentation,
        Fetch::data(
            sized_chunk(
                0.5,
                PRESENTATION_FRAMES,
                u64::try_from(OLD_FRAMES).expect("test frame count fits u64"),
                replacement_spec,
            ),
            4,
        ),
        "replacement source fits behind its barrier",
    );

    let mut saw_replacement = false;
    let mut backpressured = 0;
    for _ in 0..32 {
        match serviced_step(&mut presentation).expect("serviced decoder transition succeeds") {
            PresentResult::Produced(_) => {
                let Fetch::Data { data, .. } = input.try_pop().expect("serviced output") else {
                    panic!("expected serviced PCM");
                };
                if data.chunk().spec() == old_spec {
                    assert!(!saw_replacement);
                    assert_eq!(data.point().generation(), 0);
                    assert!(data.chunk().samples.iter().all(|sample| *sample == 0.25));
                    old_frames += data.chunk().frames();
                } else {
                    assert_eq!(data.chunk().spec(), replacement_spec);
                    assert_eq!(data.point().generation(), 1);
                    assert_eq!(old_frames, OLD_FRAMES);
                    assert!(data.chunk().samples.iter().all(|sample| *sample == 0.5));
                    saw_replacement = true;
                }
                presentation.recycle_output(data.into());
                if saw_replacement {
                    break;
                }
            }
            PresentResult::Backpressured => backpressured += 1,
            PresentResult::Advanced => {}
            PresentResult::Idle | PresentResult::Terminal => {
                panic!("serviced decoder transition stalled before replacement output")
            }
        }
    }

    assert_eq!(old_frames, OLD_FRAMES);
    assert!(
        saw_replacement,
        "replacement PCM must follow the full old chunk"
    );
    assert_eq!(backpressured, 1);
}

#[cfg(feature = "stretch-signalsmith")]
#[kithara::test]
fn serviced_region_boundary_precedes_queued_decoder_boundary() {
    let old_spec = PcmSpec::new(
        2,
        NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
    );
    let replacement_spec = PcmSpec::new(
        2,
        NonZeroU32::new(48_000).expect("test sample rate is non-zero"),
    );
    let plan = RegionPlan::new(vec![
        GridSegment::new(0, 256, 0.9),
        GridSegment::new(256, 512, 1.1),
    ])
    .expect("test region plan is valid");
    let controls = StretchControls::new(1.0);
    controls.set_backend(StretchKind::Signalsmith);
    controls.set_keylock(true);
    controls.set_region_plan(Some(Arc::new(plan)));
    let (mut presentation, mut input, frontier) = tempo_presentation(&controls, old_spec, 6);
    admit(
        &mut presentation,
        Fetch::data(sized_chunk(0.25, PRESENTATION_FRAMES, 0, old_spec), 6),
        "region source fits",
    );
    assert_eq!(
        serviced_step(&mut presentation).expect("region source admits"),
        PresentResult::Advanced
    );
    presentation
        .admit_barrier(PresentationBarrier::DecoderReplaced {
            epoch: 6,
            spec: replacement_spec,
        })
        .expect("decoder barrier queues behind region source");
    admit(
        &mut presentation,
        Fetch::data(
            sized_chunk(0.5, PRESENTATION_FRAMES, 512, replacement_spec),
            6,
        ),
        "replacement source queues behind decoder barrier",
    );
    presentation.finish_eof(6);

    let mut saw_old = false;
    let mut saw_replacement = false;
    let mut terminal = false;
    let mut backpressured = 0;
    for _ in 0..512 {
        match serviced_step(&mut presentation).expect("ordered region and decoder transitions") {
            PresentResult::Produced(_) => {
                let Fetch::Data { data, .. } = input.try_pop().expect("ordered output") else {
                    panic!("expected ordered PCM");
                };
                match data.point().generation() {
                    0 => {
                        assert!(!saw_replacement);
                        assert_eq!(data.chunk().spec(), old_spec);
                        saw_old = true;
                    }
                    1 => {
                        assert_eq!(data.chunk().spec(), replacement_spec);
                        saw_replacement = true;
                    }
                    generation => panic!("unexpected presentation generation {generation}"),
                }
                presentation.recycle_output(data.into());
            }
            PresentResult::Terminal => {
                terminal = true;
                break;
            }
            PresentResult::Backpressured => backpressured += 1,
            PresentResult::Advanced => {}
            PresentResult::Idle => {
                panic!("ordered region and decoder transitions stalled before EOF")
            }
        }
    }

    assert!(saw_old, "region source must produce before decoder reset");
    assert!(
        saw_replacement,
        "replacement source must produce after decoder reset"
    );
    assert!(terminal, "ordered transitions must reach EOF");
    assert_eq!(backpressured, 1);
    let final_point = frontier.snapshot().expect("final replacement frontier");
    assert_eq!(final_point.generation(), 1);
    assert_eq!(final_point.source_frame(), 1_024);
    assert!(matches!(
        input.try_pop(),
        Some(Fetch::NaturalEof { epoch: 6 })
    ));
}

#[kithara::test]
fn drained_decoder_boundary_waits_for_recycled_buffer_without_redraining() {
    let begins = Arc::new(AtomicUsize::new(0));
    let reconfigures = Arc::new(AtomicUsize::new(0));
    let tempo = SlowStage::new(Arc::new(AtomicUsize::new(0)), 0)
        .with_discontinuity(0, Arc::clone(&begins))
        .with_reconfigures(Arc::clone(&reconfigures));
    let (output, mut input) = connect_strict(2, None);
    let (publisher, frontier) = presentation_cell(4);
    let mut presentation = Presentation::new(
        4,
        PresentationChain {
            effects: Vec::new(),
            tempo: Some(Box::new(tempo)),
        },
        PcmPool::default(),
        spec(44_100),
        output,
        publisher,
        4,
    );
    admit(
        &mut presentation,
        Fetch::data(chunk(1.0, 0), 4),
        "old raw block fits",
    );
    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    for _ in 0..2 {
        assert!(matches!(
            presentation.step(|_| {}),
            Ok(PresentResult::Produced(_))
        ));
    }
    let _ = data(
        &mut presentation,
        input.try_pop().expect("first old output"),
    );
    let held = input
        .try_pop()
        .expect("second old output remains checked out");

    presentation
        .admit_barrier(PresentationBarrier::DecoderReplaced {
            epoch: 4,
            spec: spec(48_000),
        })
        .expect("decoder barrier fits");
    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    assert_eq!(begins.load(Ordering::Acquire), 1);
    assert_eq!(step(&mut presentation), PresentResult::Backpressured);
    assert_eq!(step(&mut presentation), PresentResult::Backpressured);
    assert_eq!(begins.load(Ordering::Acquire), 1);
    assert_eq!(reconfigures.load(Ordering::Acquire), 0);

    let Fetch::Data { data: held, .. } = held else {
        panic!("held output must contain PCM");
    };
    presentation.recycle_output(held.into());
    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    assert_eq!(reconfigures.load(Ordering::Acquire), 1);
    assert_eq!(
        frontier
            .snapshot()
            .expect("decoder frontier is republished")
            .generation(),
        1
    );
}

#[kithara::test]
fn terminal_with_queued_decoder_barrier_is_prepared_before_eof() {
    let reconfigures = Arc::new(AtomicUsize::new(0));
    let tempo = SlowStage::new(Arc::new(AtomicUsize::new(0)), 0)
        .with_reconfigures(Arc::clone(&reconfigures));
    let (output, mut input) = connect_strict(1, None);
    let (publisher, frontier) = presentation_cell(4);
    let mut presentation = Presentation::new(
        2,
        PresentationChain {
            effects: Vec::new(),
            tempo: Some(Box::new(tempo)),
        },
        PcmPool::default(),
        spec(44_100),
        output,
        publisher,
        4,
    );
    presentation
        .admit_barrier(PresentationBarrier::DecoderReplaced {
            epoch: 4,
            spec: spec(48_000),
        })
        .expect("queued decoder barrier fits");
    presentation.finish_eof(4);

    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    assert_eq!(step(&mut presentation), PresentResult::Backpressured);
    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    assert_eq!(reconfigures.load(Ordering::Acquire), 1);
    assert!(frontier.snapshot().is_none());
    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    assert_eq!(step(&mut presentation), PresentResult::Terminal);
    assert!(matches!(
        input.try_pop(),
        Some(Fetch::NaturalEof { epoch: 4 })
    ));
}

#[kithara::test]
fn failure_resets_held_tempo_source_without_discontinuity_drain() {
    let begins = Arc::new(AtomicUsize::new(0));
    let reconfigures = Arc::new(AtomicUsize::new(0));
    let tempo = SlowStage::new(Arc::new(AtomicUsize::new(0)), 0)
        .with_discontinuity(700, Arc::clone(&begins))
        .with_reconfigures(Arc::clone(&reconfigures));
    let (output, mut input) = connect_strict(1, None);
    let (publisher, _) = presentation_cell(0);
    let mut presentation = Presentation::new(
        2,
        PresentationChain {
            effects: Vec::new(),
            tempo: Some(Box::new(tempo)),
        },
        PcmPool::default(),
        spec(44_100),
        output,
        publisher,
        0,
    );
    admit(
        &mut presentation,
        Fetch::data(chunk(1.0, 0), 0),
        "raw block fits",
    );
    presentation.finish_failed(0);

    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    for _ in 0..2 {
        assert!(matches!(
            presentation.step(|_| {}),
            Ok(PresentResult::Produced(_))
        ));
        let _ = data(&mut presentation, input.try_pop().expect("prior tempo PCM"));
    }
    assert_eq!(step(&mut presentation), PresentResult::Terminal);
    assert!(matches!(input.try_pop(), Some(Fetch::Failure { epoch: 0 })));
    assert_eq!(begins.load(Ordering::Acquire), 0);
    assert_eq!(reconfigures.load(Ordering::Acquire), 0);
}
