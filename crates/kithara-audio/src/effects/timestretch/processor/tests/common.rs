use std::num::NonZero;

use kithara_bufpool::PcmPool;
use kithara_decode::{DecodeError, DecodeResult, PcmChunk, PcmMeta, PcmSpec, duration_for_frames};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_stretch::StretchKind;
use realfft::RealFftPlanner;

use super::super::*;

pub(super) struct Consts;

impl Consts {
    pub(super) const CH: u16 = 2;
    pub(super) const F0: f64 = 440.0;
    pub(super) const N: usize = 1 << 14;
    pub(super) const SR: u32 = 44_100;
}

pub(super) fn f32_of(x: f64) -> f32 {
    num_traits::cast(x).unwrap_or_default()
}

pub(super) fn f64_of(x: usize) -> f64 {
    num_traits::cast(x).unwrap_or_default()
}

pub(super) fn sine(frames: usize) -> Vec<f32> {
    let inc = std::f64::consts::TAU * Consts::F0 / f64::from(Consts::SR);
    let mut phase = 0.0_f64;
    let mut out = Vec::with_capacity(frames * usize::from(Consts::CH));
    for _ in 0..frames {
        let s = f32_of(0.5 * phase.sin());
        out.push(s);
        out.push(s);
        phase += inc;
    }
    out
}

pub(super) fn chunk(samples: &[f32]) -> PcmChunk {
    chunk_at(spec(), samples)
}

pub(super) fn chunk_at(spec: PcmSpec, samples: &[f32]) -> PcmChunk {
    let frames = samples.len() / usize::from(Consts::CH);
    PcmChunk::new(
        PcmMeta {
            spec,
            frames: u32::try_from(frames).unwrap_or(0),
            timestamp: Duration::ZERO,
            ..Default::default()
        },
        PcmPool::default().attach(samples.to_vec()),
    )
}

pub(super) fn dominant_bin(mono: &[f32]) -> usize {
    let start = (mono.len().saturating_sub(Consts::N)) / 2;
    let seg = &mono[start..start + Consts::N];
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(Consts::N);
    let mut input = fft.make_input_vec();
    input.copy_from_slice(seg);
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut input, &mut spectrum).unwrap();
    spectrum
        .iter()
        .enumerate()
        .skip(1)
        .max_by(|a, b| a.1.norm().total_cmp(&b.1.norm()))
        .map_or(0, |(i, _)| i)
}

pub(super) fn expected_bin(freq: f64) -> usize {
    num_traits::cast((freq * f64_of(Consts::N) / f64::from(Consts::SR)).round()).unwrap_or(0)
}

pub(super) fn spec() -> PcmSpec {
    PcmSpec {
        channels: Consts::CH,
        sample_rate: NonZero::new(Consts::SR).unwrap(),
    }
}

pub(super) fn processor(controls: Arc<StretchControls>) -> TimeStretchProcessor {
    let mut processor = TimeStretchProcessor::new(controls, spec(), PcmPool::default().clone());
    TempoStage::service_off_rt(
        &mut processor,
        TempoPrepareRequest::Current { spec: spec() },
    )
    .expect("fixture tempo core prepares off-RT");
    processor
}

pub(in crate::effects::timestretch) fn render_chunk(
    stage: &mut TimeStretchProcessor,
    chunk: PcmChunk,
) -> DecodeResult<Option<PcmChunk>> {
    let spec = chunk.spec();
    let mut rendered = Vec::new();
    service_boundary(stage, spec, &mut rendered)?;
    let source_meta = chunk.meta;
    let channels = stage.channels();
    for (part_index, samples) in chunk
        .samples
        .chunks(TimeStretchProcessor::PRESENTATION_FRAMES * channels)
        .enumerate()
    {
        let source_offset = part_index
            .checked_mul(TimeStretchProcessor::PRESENTATION_FRAMES)
            .ok_or(DecodeError::InvalidData {
                detail: "fixture tempo source offset overflow",
            })?;
        let frames = samples.len() / channels;
        let mut meta = source_meta;
        meta.frame_offset = meta
            .frame_offset
            .checked_add(
                u64::try_from(source_offset).map_err(|_| DecodeError::InvalidData {
                    detail: "fixture tempo source offset exceeds u64",
                })?,
            )
            .ok_or(DecodeError::InvalidData {
                detail: "fixture tempo source position overflow",
            })?;
        let source_offset = u64::try_from(source_offset).map_err(|_| DecodeError::InvalidData {
            detail: "fixture tempo source offset exceeds u64",
        })?;
        meta.timestamp = source_meta.timestamp.saturating_add(duration_for_frames(
            stage.output_spec().sample_rate.get(),
            source_offset,
        ));
        meta.frames = u32::try_from(frames).map_err(|_| DecodeError::InvalidData {
            detail: "fixture tempo source length exceeds u32",
        })?;
        let part = PcmChunk::new(meta, PcmPool::default().attach(samples.to_vec()));
        TempoStage::push_source(stage, part)?;
        while TempoStage::buffered_source_quanta(stage) != 0 {
            if service_boundary(stage, spec, &mut rendered)? {
                continue;
            }
            let mut output = vec![0.0; TimeStretchProcessor::PRESENTATION_FRAMES * channels];
            let credit = OutputCredit::new(
                &mut output,
                channels,
                TimeStretchProcessor::PRESENTATION_FRAMES,
            );
            if let TempoStep::Rendered { frames, .. } =
                TempoStage::render(stage, None, credit, &mut |_| {})?
            {
                rendered.extend_from_slice(&output[..frames * channels]);
            }
        }
    }
    if rendered.is_empty() {
        return Ok(None);
    }
    let mut meta = source_meta;
    meta.frames =
        u32::try_from(rendered.len() / channels).map_err(|_| DecodeError::InvalidData {
            detail: "fixture tempo output length exceeds u32",
        })?;
    Ok(Some(PcmChunk::new(meta, stage.pool.attach(rendered))))
}

fn service_boundary(
    stage: &mut TimeStretchProcessor,
    spec: PcmSpec,
    rendered: &mut Vec<f32>,
) -> DecodeResult<bool> {
    TempoStage::service_off_rt(stage, TempoPrepareRequest::Current { spec })?;
    if let Some(boundary) = TempoStage::prepared_boundary(stage) {
        let mut debt = TempoStage::begin_discontinuity(stage)?;
        loop {
            let channels = stage.channels();
            let mut output = vec![0.0; TimeStretchProcessor::PRESENTATION_FRAMES * channels];
            let credit = OutputCredit::new(
                &mut output,
                channels,
                TimeStretchProcessor::PRESENTATION_FRAMES,
            );
            match TempoStage::render_discontinuity(stage, &mut debt, credit, &mut |_| {})? {
                TempoDiscontinuityStep::Drained => break,
                TempoDiscontinuityStep::Rendered { frames, .. } => {
                    rendered.extend_from_slice(&output[..frames * channels]);
                }
            }
        }
        TempoStage::commit_prepared(stage, boundary)?;
        return Ok(true);
    }
    Ok(false)
}

pub(in crate::effects::timestretch) fn drain_eof(
    stage: &mut TimeStretchProcessor,
) -> Option<PcmChunk> {
    let mut debt = TempoStage::finish_eof(stage).ok()?;
    let channels = stage.channels();
    let mut rendered = Vec::new();
    loop {
        let mut output = vec![0.0; TimeStretchProcessor::PRESENTATION_FRAMES * channels];
        let credit = OutputCredit::new(
            &mut output,
            channels,
            TimeStretchProcessor::PRESENTATION_FRAMES,
        );
        match TempoStage::render_eof(stage, &mut debt, credit, &mut |_| {}) {
            Ok(TempoEofStep::Drained) => break,
            Ok(TempoEofStep::Rendered { frames, .. }) => {
                rendered.extend_from_slice(&output[..frames * channels]);
            }
            Err(_) => return None,
        }
    }
    if rendered.is_empty() {
        return None;
    }
    let spec = stage.output_spec();
    let mut meta = stage.last_input_meta?;
    meta.spec = spec;
    meta.frames = u32::try_from(rendered.len() / channels).ok()?;
    Some(PcmChunk::new(meta, stage.pool.attach(rendered)))
}

pub(in crate::effects::timestretch) fn source_endpoint(
    stage: &TimeStretchProcessor,
) -> Option<u64> {
    let meta = stage.last_input_meta?;
    meta.frame_offset.checked_add(u64::from(meta.frames))
}

pub(super) fn keylocked(kind: StretchKind, speed: f32) -> TimeStretchProcessor {
    let controls = StretchControls::new(speed);
    controls.set_keylock(true);
    controls.set_backend(kind);
    processor(controls)
}

pub(super) fn vinyl(kind: StretchKind, speed: f32) -> TimeStretchProcessor {
    let controls = StretchControls::new(speed);
    controls.set_keylock(false);
    controls.set_backend(kind);
    processor(controls)
}

pub(super) fn render(fx: &mut TimeStretchProcessor, input: &[f32]) -> Vec<f32> {
    let mut out: Vec<f32> = Vec::new();
    let block = 4096 * usize::from(Consts::CH);
    for data in input.chunks(block) {
        if let Some(c) =
            render_chunk(fx, chunk(data)).expect("fixture stretch processing must succeed")
        {
            assert_eq!(
                c.spec().sample_rate.get(),
                Consts::SR,
                "stretch preserves sample rate"
            );
            assert_eq!(c.spec().channels, Consts::CH);
            out.extend_from_slice(&c.samples);
        }
    }
    while let Some(c) = drain_eof(fx) {
        // A non-empty flush chunk carries real audio, so its spec must stay
        // the source spec, never the `PcmMeta::default()` sentinel (0
        // channels) that a `None` `last_input_meta` would otherwise yield.
        assert_eq!(c.spec().channels, Consts::CH, "flush preserves channels");
        assert_eq!(
            c.spec().sample_rate.get(),
            Consts::SR,
            "flush preserves sample rate"
        );
        out.extend_from_slice(&c.samples);
    }
    out
}

pub(super) fn run_keylocked(kind: StretchKind, speed: f32, in_frames: usize) -> Vec<f32> {
    let input = sine(in_frames);
    render(&mut keylocked(kind, speed), &input)
}

pub(super) fn run_vinyl(kind: StretchKind, speed: f32, in_frames: usize) -> Vec<f32> {
    let input = sine(in_frames);
    render(&mut vinyl(kind, speed), &input)
}

pub(super) fn assert_half_speed_contract(kind: StretchKind) {
    let channels = usize::from(Consts::CH);
    let in_frames = usize::try_from(Consts::SR).unwrap() * 2; // 2 s
    let out = run_keylocked(kind, 0.5, in_frames);
    let out_frames = out.len() / channels;

    // Both C++ backends emit fixed-length output with leading latency-fill
    // (and bungee drops its tail), nudging the measured duration off an
    // exact 2x on a short clip, hence the 10% band.
    assert!(
        out_frames * 10 >= in_frames * 18 && out_frames * 10 <= in_frames * 22,
        "{kind:?}: expected ~2x duration, got {out_frames} from {in_frames}"
    );

    // Pitch preserved: dominant bin still at F0 (the load-bearing check;
    // a resampler-in-disguise would shift it).
    let mono: Vec<f32> = out.iter().step_by(channels).copied().collect();
    assert!(
        mono.len() >= Consts::N,
        "{kind:?}: not enough output for the FFT window"
    );
    let peak = dominant_bin(&mono);
    let want = expected_bin(Consts::F0);
    assert!(
        peak.abs_diff(want) <= 3,
        "{kind:?}: pitch moved under time-stretch: peak bin {peak}, expected {want}"
    );
}

pub(super) fn assert_unity_contract(kind: StretchKind) {
    let in_frames = usize::try_from(Consts::SR).unwrap() * 2;
    let input = sine(in_frames);
    let out = render(&mut keylocked(kind, 1.0), &input);
    assert_eq!(out, input, "{kind:?}: unity speed must bypass byte-exact");
}
