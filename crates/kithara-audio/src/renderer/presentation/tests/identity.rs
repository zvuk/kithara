use super::*;

#[kithara::test]
fn identity_path_is_frame_preserving_without_tempo() {
    let seen = Arc::new(AtomicUsize::new(0));
    let (mut presentation, mut input) = identity_presentation(vec![Box::new(StampEffect {
        resets: None,
        seen_frames: Arc::clone(&seen),
        value: 3.0,
    })]);
    admit(
        &mut presentation,
        Fetch::data(chunk(1.0, 0), 0),
        "raw chunk fits",
    );

    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(PresentedBlock {
            frames: PRESENTATION_FRAMES,
            ..
        }))
    ));
    let output = data(&mut presentation, input.try_pop().expect("identity output"));
    assert_eq!(output.frames(), PRESENTATION_FRAMES);
    assert_eq!(output.samples[0], 3.0);
    assert_eq!(seen.load(Ordering::Acquire), PRESENTATION_FRAMES);
}

#[kithara::test]
fn identity_path_commits_a_short_decoder_chunk_without_padding() {
    const FRAMES: usize = 128;
    let stereo = PcmSpec::new(
        2,
        NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
    );
    let (output, mut input) = connect_strict(1, None);
    let (publisher, _) = presentation_cell(0);
    let mut presentation = Presentation::new(
        1,
        PresentationChain::identity(Vec::new()),
        PcmPool::default(),
        stereo,
        output,
        publisher,
        0,
    );
    let mut meta = PcmMeta::default();
    meta.spec = stereo;
    meta.frames = u32::try_from(FRAMES).expect("test frame count fits u32");
    let samples = FRAMES * usize::from(stereo.channels);
    let chunk = PcmChunk::new(meta, PcmPool::default().attach(vec![0.25; samples]));
    admit(
        &mut presentation,
        Fetch::data(chunk, 0),
        "short raw chunk fits",
    );

    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(PresentedBlock {
            frames: FRAMES,
            ..
        }))
    ));
    let output = data(
        &mut presentation,
        input.try_pop().expect("short output is committed"),
    );
    assert_eq!(output.frames(), FRAMES);
    assert_eq!(output.samples.len(), samples);
    assert!(output.samples.iter().all(|sample| *sample == 0.25));
}

#[kithara::test]
fn underprovisioned_shared_pool_is_reserved_before_identity_rt_step() {
    let pool = PcmPool::new(128, 200_000);
    pool.pre_warm(1, |buffer| buffer.resize(1, 0.0));
    assert_ne!(pool.allocated_bytes(), 0, "fixture pool starts non-empty");
    let (output, mut input) = connect_strict(1, None);
    let (publisher, _) = presentation_cell(0);
    let mut presentation = Presentation::new(
        1,
        PresentationChain::identity(Vec::new()),
        pool.clone(),
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
    let misses = pool.stats().alloc_misses;
    let mut retired = None;

    assert!(matches!(
        guarded_step(&mut presentation, &mut retired),
        Ok(PresentResult::Produced(_))
    ));
    assert_eq!(pool.stats().alloc_misses, misses);

    drop(retired.take());
    let Fetch::Data { data, .. } = input.try_pop().expect("identity output") else {
        panic!("expected identity PCM");
    };
    presentation.recycle_output(data.into());
}

#[kithara::test]
fn tempo_rt_step_uses_only_the_local_output_reserve() {
    let pool = PcmPool::new(128, 200_000);
    pool.pre_warm(1, |buffer| buffer.resize(8, 0.0));
    let tempo = SlowStage::new(Arc::new(AtomicUsize::new(0)), 0);
    let (output, mut input) = connect_strict(1, None);
    let (publisher, _) = presentation_cell(0);
    let mut presentation = Presentation::new(
        1,
        PresentationChain {
            effects: Vec::new(),
            tempo: Some(Box::new(tempo)),
        },
        pool.clone(),
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
    let misses = pool.stats().alloc_misses;
    let mut retired = None;

    assert_eq!(
        guarded_step(&mut presentation, &mut retired).expect("tempo admit step"),
        PresentResult::Advanced
    );
    assert!(matches!(
        guarded_step(&mut presentation, &mut retired),
        Ok(PresentResult::Produced(_))
    ));
    assert_eq!(pool.stats().alloc_misses, misses);

    drop(retired.take());
    let Fetch::Data { data, .. } = input.try_pop().expect("tempo output") else {
        panic!("expected tempo PCM");
    };
    presentation.recycle_output(data.into());
}

#[kithara::test]
fn exhausted_local_reserve_backpressures_until_consumer_recycle() {
    let (output, mut input) = connect_strict(2, None);
    let (publisher, _) = presentation_cell(0);
    let mut presentation = Presentation::new(
        4,
        PresentationChain::identity(Vec::new()),
        PcmPool::default(),
        spec(44_100),
        output,
        publisher,
        0,
    );
    for block in 0_u16..4 {
        admit(
            &mut presentation,
            Fetch::data(chunk(f32::from(block), u64::from(block) * 512), 0),
            "raw block fits",
        );
    }
    assert!(matches!(
        step(&mut presentation),
        PresentResult::Produced(_)
    ));
    assert!(matches!(
        step(&mut presentation),
        PresentResult::Produced(_)
    ));
    let returned = input.try_pop().expect("consumer owns first current block");
    assert!(matches!(
        step(&mut presentation),
        PresentResult::Produced(_)
    ));
    let current = input
        .try_pop()
        .expect("consumer advances to second current block");

    assert_eq!(step(&mut presentation), PresentResult::Backpressured);

    let Fetch::Data { data, .. } = returned else {
        panic!("expected returned PCM");
    };
    presentation.recycle_output(data.into());
    assert!(matches!(
        step(&mut presentation),
        PresentResult::Produced(_)
    ));

    let Fetch::Data { data, .. } = current else {
        panic!("expected current PCM");
    };
    presentation.recycle_output(data.into());
    while let Some(Fetch::Data { data, .. }) = input.try_pop() {
        presentation.recycle_output(data.into());
    }
}

#[kithara::test]
fn final_payload_carries_the_exact_committed_presentation_point() {
    let (output, mut input) = connect_strict(1, None);
    let (publisher, frontier) = presentation_cell(0);
    let mut presentation = Presentation::new(
        1,
        PresentationChain::identity(Vec::new()),
        PcmPool::default(),
        spec(44_100),
        output,
        publisher,
        0,
    );
    admit(
        &mut presentation,
        Fetch::data(chunk(1.0, 256), 0),
        "raw chunk fits",
    );

    assert!(matches!(
        presentation.step(|_| {}),
        Ok(PresentResult::Produced(_))
    ));
    let Fetch::Data { data, .. } = input.try_pop().expect("final output is committed") else {
        panic!("expected final PCM data");
    };
    let payload_point = data.point();
    let frontier_point = frontier.point(0).expect("frontier is committed");
    assert_eq!(payload_point, frontier_point);
    assert_eq!(payload_point.seek_epoch(), 0);
    assert_eq!(payload_point.source_frame(), 768);
    assert_eq!(payload_point.generation(), 0);
    assert_eq!(payload_point.output_end(), 512);
    assert_eq!(payload_point.sample_rate().get(), 44_100);
}

#[kithara::test]
fn source_progress_ignores_decoder_timestamp_gaps_after_the_first_chunk() {
    let (output, mut input) = connect_strict(1, None);
    let (publisher, _) = presentation_cell(0);
    let mut presentation = Presentation::new(
        2,
        PresentationChain::identity(Vec::new()),
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
    admit(
        &mut presentation,
        Fetch::data(chunk(2.0, 1_173), 0),
        "timestamp-gapped raw chunk fits",
    );

    assert!(matches!(
        step(&mut presentation),
        PresentResult::Produced(_)
    ));
    let Fetch::Data { data: first, .. } = input.try_pop().expect("first output") else {
        panic!("expected first output data");
    };
    assert_eq!(first.point().source_frame(), 512);
    presentation.recycle_output(first.into());

    assert!(matches!(
        step(&mut presentation),
        PresentResult::Produced(_)
    ));
    let Fetch::Data { data: second, .. } = input.try_pop().expect("second output") else {
        panic!("expected second output data");
    };
    assert_eq!(
        second.point().source_frame(),
        1_024,
        "presentation source progress follows admitted PCM, not decoder head-strip gaps"
    );
    presentation.recycle_output(second.into());
}

#[kithara::test]
fn same_rate_decoder_barrier_preserves_cumulative_admitted_source_end() {
    let (output, mut input) = connect_strict(1, None);
    let (publisher, _) = presentation_cell(0);
    let mut presentation = Presentation::new(
        3,
        PresentationChain::identity(Vec::new()),
        PcmPool::default(),
        spec(44_100),
        output,
        publisher,
        0,
    );
    admit(
        &mut presentation,
        Fetch::data(chunk(1.0, 0), 0),
        "old decoder chunk fits",
    );
    presentation
        .admit_barrier(PresentationBarrier::DecoderReplaced {
            epoch: 0,
            spec: spec(44_100),
        })
        .expect("same-rate decoder barrier fits");
    admit(
        &mut presentation,
        Fetch::data(chunk(2.0, 1_173), 0),
        "replacement decoder chunk fits",
    );

    assert!(matches!(
        step(&mut presentation),
        PresentResult::Produced(_)
    ));
    let Fetch::Data { data: old, .. } = input.try_pop().expect("old decoder output") else {
        panic!("expected old decoder data");
    };
    assert_eq!(old.point().generation(), 0);
    assert_eq!(old.point().source_frame(), 512);
    presentation.recycle_output(old.into());

    assert_eq!(step(&mut presentation), PresentResult::Advanced);
    assert!(matches!(
        step(&mut presentation),
        PresentResult::Produced(_)
    ));
    let Fetch::Data {
        data: replacement, ..
    } = input.try_pop().expect("replacement decoder output")
    else {
        panic!("expected replacement decoder data");
    };
    assert_eq!(replacement.point().generation(), 1);
    assert_eq!(
        replacement.point().source_frame(),
        1_024,
        "same-rate replacement continues from admitted source end instead of decoder timestamps"
    );
    presentation.recycle_output(replacement.into());
}

#[kithara::test]
fn custom_effect_receives_fixed_shape_block() {
    let seen = Arc::new(AtomicUsize::new(0));
    let (mut presentation, mut input) = identity_presentation(vec![Box::new(StampEffect {
        resets: None,
        seen_frames: Arc::clone(&seen),
        value: 4.0,
    })]);
    admit(
        &mut presentation,
        Fetch::data(chunk(0.0, 0), 0),
        "raw chunk fits",
    );
    let _ = presentation.step(|_| {});

    let output = data(
        &mut presentation,
        input.try_pop().expect("frame-preserving output"),
    );
    assert_eq!(seen.load(Ordering::Acquire), output.frames());
    assert_eq!(
        usize::try_from(output.meta.frames).expect("test frame count fits usize"),
        output.frames()
    );
}
