use super::*;

#[kithara::test]
fn slow_tempo_never_exceeds_one_output_credit() {
    let max_credit = Arc::new(AtomicUsize::new(0));
    let tempo = SlowStage::new(Arc::clone(&max_credit), 0);
    let (output, mut input) = connect_strict(1, None);
    let (publisher, _) = presentation_cell(0);
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
        0,
    );
    admit(
        &mut presentation,
        Fetch::data(chunk(2.0, 0), 0),
        "raw chunk fits",
    );

    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(_))
    ));
    assert_eq!(
        data(
            &mut presentation,
            input.try_pop().expect("first slow block"),
        )
        .frames(),
        512
    );
    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(_))
    ));
    assert_eq!(
        data(
            &mut presentation,
            input.try_pop().expect("second slow block"),
        )
        .frames(),
        512
    );
    assert_eq!(max_credit.load(Ordering::Acquire), PRESENTATION_FRAMES);
}

#[kithara::test]
fn deep_raw_buffer_does_not_become_post_effect_latency() {
    let tempo = SlowStage::new(Arc::new(AtomicUsize::new(0)), 0);
    let (output, mut input) = connect_strict(1, None);
    let (publisher, _) = presentation_cell(0);
    let mut presentation = Presentation::new(
        10,
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
    for block in 0_u16..10 {
        admit(
            &mut presentation,
            Fetch::data(chunk(f32::from(block), u64::from(block) * 512), 0),
            "all ten raw blocks fit before presentation",
        );
    }
    assert!(presentation.is_raw_full());

    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(PresentedBlock { frames: 512, .. }))
    ));
    let first = data(
        &mut presentation,
        input
            .try_pop()
            .expect("first processed block is final output"),
    );
    assert_eq!(first.samples[0], 0.0);
    assert!(presentation.is_raw_full());
}

#[kithara::test]
fn tempo_source_quantum_remains_charged_until_consumed() {
    let tempo = SlowStage::new(Arc::new(AtomicUsize::new(0)), 0);
    let (output, _input) = connect_strict(1, None);
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
        "first raw chunk fits",
    );
    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    admit(
        &mut presentation,
        Fetch::data(chunk(2.0, 512), 0),
        "one queued plus one stage-held chunk fits",
    );

    assert!(presentation.is_raw_full());
    assert!(
        presentation
            .admit(Fetch::data(chunk(3.0, 1_024), 0))
            .is_some(),
        "stage-held source remains charged to the deep capacity",
    );
}

#[kithara::test]
fn tempo_render_receives_the_current_canonical_presentation_point() {
    let seen = Arc::new(AtomicU64::new(u64::MAX));
    let tempo = SlowStage::new(Arc::new(AtomicUsize::new(0)), 0)
        .with_seen_presentation_output_end(Arc::clone(&seen));
    let (output, mut input) = connect_strict(1, None);
    let (publisher, _) = presentation_cell(0);
    let mut presentation = Presentation::new(
        1,
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
        "source chunk fits",
    );

    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(_))
    ));
    assert_eq!(seen.load(Ordering::Acquire), 0);
    assert!(input.try_pop().is_some());
    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(_))
    ));
    assert_eq!(seen.load(Ordering::Acquire), 512);
}

#[kithara::test]
fn eof_tail_drains_only_through_output_credits() {
    let max_credit = Arc::new(AtomicUsize::new(0));
    let tempo = SlowStage::new(Arc::clone(&max_credit), 700);
    let (output, mut input) = connect_strict(1, None);
    let (publisher, _) = presentation_cell(7);
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
        7,
    );
    admit(
        &mut presentation,
        Fetch::data(chunk(1.0, 0), 7),
        "final source chunk fits",
    );
    presentation.finish_eof(7);

    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    for _ in 0..2 {
        assert!(matches!(
            presentation.step(|_| {}),
            Ok(PresentResult::Produced(PresentedBlock { frames: 512, .. }))
        ));
        let source = data(
            &mut presentation,
            input.try_pop().expect("source block before EOF debt"),
        );
        assert_eq!(source.frames(), 512);
        assert_eq!(source.samples[0], 1.0);
    }
    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(PresentedBlock { frames: 512, .. }))
    ));
    assert_eq!(
        data(
            &mut presentation,
            input.try_pop().expect("first EOF debt block"),
        )
        .frames(),
        512
    );
    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(PresentedBlock { frames: 188, .. }))
    ));
    assert_eq!(
        data(
            &mut presentation,
            input.try_pop().expect("second EOF debt block"),
        )
        .frames(),
        188
    );
    assert_eq!(step(&mut presentation), PresentResult::Terminal);
    assert!(matches!(
        input.try_pop(),
        Some(Fetch::NaturalEof { epoch: 7 })
    ));
    assert_eq!(max_credit.load(Ordering::Acquire), PRESENTATION_FRAMES);
}

#[cfg(feature = "stretch-signalsmith")]
#[kithara::test]
fn signalsmith_unity_keylock_accepts_short_decoder_chunks() {
    const FRAMES: usize = 128;
    let source_spec = PcmSpec::new(
        2,
        NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
    );
    let pool = PcmPool::new(128, 200_000);
    let controls = StretchControls::new(1.0);
    controls.set_keylock(true);
    controls.set_backend(StretchKind::Signalsmith);
    let chain = create_presentation_chain(source_spec, Some(&controls), &pool, Vec::new());
    let (output, mut input) = connect_strict(2, None);
    let (publisher, _) = presentation_cell(9);
    let mut presentation = Presentation::new(4, chain, pool, source_spec, output, publisher, 9);
    let mut produced = 0;

    for chunk_index in 0..16_u64 {
        let mut meta = PcmMeta::default();
        meta.spec = source_spec;
        meta.frames = u32::try_from(FRAMES).expect("test frame count fits u32");
        meta.frame_offset = chunk_index * u64::try_from(FRAMES).expect("test frames fit u64");
        let samples = vec![0.25; FRAMES * usize::from(source_spec.channels)];
        admit(
            &mut presentation,
            Fetch::data(PcmChunk::new(meta, PcmPool::default().attach(samples)), 9),
            "short source chunk fits",
        );
        for _ in 0..4 {
            match step(&mut presentation) {
                PresentResult::Produced(_) => {
                    produced += 1;
                    let _ = data(
                        &mut presentation,
                        input.try_pop().expect("short tempo output"),
                    );
                }
                PresentResult::Idle => break,
                PresentResult::Advanced => {}
                PresentResult::Backpressured | PresentResult::Terminal => {
                    panic!("short source unexpectedly stopped presentation")
                }
            }
        }
    }
    presentation.finish_eof(9);
    for _ in 0..64 {
        match step(&mut presentation) {
            PresentResult::Produced(_) => {
                produced += 1;
                let _ = data(
                    &mut presentation,
                    input.try_pop().expect("short tempo tail output"),
                );
            }
            PresentResult::Terminal => break,
            PresentResult::Advanced | PresentResult::Idle => {}
            PresentResult::Backpressured => panic!("short tempo tail exhausted its reserve"),
        }
    }
    assert!(produced > 0, "short tempo input must produce audible PCM");
}

#[cfg(feature = "stretch-signalsmith")]
#[kithara::test]
fn tempo_stage_splits_large_decoder_chunks_at_the_rt_budget() {
    const FRAMES: usize = 1_152;
    let source_spec = PcmSpec::new(
        2,
        NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
    );
    let pool = PcmPool::new(128, 200_000);
    let controls = StretchControls::new(1.0);
    controls.set_keylock(true);
    controls.set_backend(StretchKind::Signalsmith);
    let chain = create_presentation_chain(source_spec, Some(&controls), &pool, Vec::new());
    let (output, mut input) = connect_strict(2, None);
    let (publisher, _) = presentation_cell(9);
    let mut presentation = Presentation::new(1, chain, pool, source_spec, output, publisher, 9);
    let mut meta = PcmMeta::default();
    meta.spec = source_spec;
    meta.frames = u32::try_from(FRAMES).expect("test frame count fits u32");
    let samples = FRAMES * usize::from(source_spec.channels);
    admit(
        &mut presentation,
        Fetch::data(
            PcmChunk::new(meta, PcmPool::default().attach(vec![0.25; samples])),
            9,
        ),
        "large decoder chunk fits the raw queue",
    );

    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    let mut block_frames = Vec::new();
    for _ in 0..3 {
        let PresentResult::Produced(block) = step(&mut presentation) else {
            panic!("each RT step must commit one bounded output block");
        };
        block_frames.push(block.frames);
        let output = data(
            &mut presentation,
            input.try_pop().expect("bounded tempo output is committed"),
        );
        assert_eq!(output.frames(), block.frames);
        assert!(output.samples.iter().all(|sample| *sample == 0.25));
    }

    assert_eq!(block_frames, [512, 512, 128]);
    assert_eq!(step(&mut presentation), PresentResult::Idle);
}

#[cfg(feature = "stretch-signalsmith")]
#[kithara::test]
fn signalsmith_tail_payloads_advance_to_the_admitted_source_end() {
    let source_spec = PcmSpec::new(
        2,
        NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
    );
    let pool = PcmPool::new(128, 200_000);
    let controls = StretchControls::new(0.5);
    controls.set_keylock(true);
    controls.set_backend(StretchKind::Signalsmith);
    let chain = create_presentation_chain(source_spec, Some(&controls), &pool, Vec::new());
    let (output, mut input) = connect_strict(1, None);
    let (publisher, _) = presentation_cell(9);
    let mut presentation = Presentation::new(1, chain, pool, source_spec, output, publisher, 9);
    let start = 2_048_u64;
    let mut meta = PcmMeta::default();
    meta.spec = source_spec;
    let presentation_frames = u32::try_from(PRESENTATION_FRAMES).expect("frame count fits u32");
    meta.frames = presentation_frames;
    meta.frame_offset = start;
    meta.end_timestamp = duration_for_frames(
        source_spec.sample_rate.get(),
        start + u64::from(presentation_frames),
    );
    let samples = vec![0.25; PRESENTATION_FRAMES * 2];
    admit(
        &mut presentation,
        Fetch::data(PcmChunk::new(meta, PcmPool::default().attach(samples)), 9),
        "source chunk fits",
    );
    presentation.finish_eof(9);

    let mut tail_points = Vec::new();
    for _ in 0..64 {
        let draining = presentation.eof_debt.is_some();
        match step(&mut presentation) {
            PresentResult::Produced(_) => {
                let Fetch::Data { data, .. } = input.try_pop().expect("presented payload") else {
                    panic!("expected presented PCM");
                };
                if draining {
                    tail_points.push(data.point());
                }
                presentation.recycle_output(data.into());
            }
            PresentResult::Terminal => break,
            PresentResult::Advanced | PresentResult::Idle | PresentResult::Backpressured => {}
        }
    }

    assert!(
        tail_points.len() > 1,
        "fixture must emit a multi-block tail"
    );
    assert!(
        tail_points
            .windows(2)
            .all(|pair| pair[0].source_frame() <= pair[1].source_frame()),
        "every tail payload source coordinate is monotonic"
    );
    assert_eq!(
        tail_points.last().map(|point| point.source_frame()),
        Some(start + u64::from(presentation_frames))
    );
    assert!(matches!(
        input.try_pop(),
        Some(Fetch::NaturalEof { epoch: 9 })
    ));
}
