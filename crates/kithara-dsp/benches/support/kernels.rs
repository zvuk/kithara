#[derive(Clone, Copy)]
enum LastLoudImplementation {
    Native,
    Rposition,
}

#[derive(Clone, Copy)]
enum LastLoudShape {
    Silent,
    Last,
}

struct LastLoudPrepared {
    samples: Vec<f32>,
    search: fn(&[f32], f32) -> Option<usize>,
}

fn prepare_last_loud(
    size: usize,
    shape: LastLoudShape,
    implementation: LastLoudImplementation,
) -> LastLoudPrepared {
    let mut samples = vec![0.0_f32; size];
    if matches!(shape, LastLoudShape::Last) {
        *samples.last_mut().expect("nonempty") = 0.25;
    }
    let search = match implementation {
        LastLoudImplementation::Native => kithara_dsp::last_loud,
        LastLoudImplementation::Rposition => loud_scalar,
    };
    assert_eq!(search(&samples, 1e-6), loud_scalar(&samples, 1e-6));
    LastLoudPrepared { samples, search }
}

#[metabench::benchmark(KERNEL_LAST_LOUD, "last_loud", "case", gungraun_setup = prepare_last_loud)]
#[bench::case_silent_native_128(args = (128_usize, LastLoudShape::Silent, LastLoudImplementation::Native))]
#[bench::case_silent_rposition_128(args = (128_usize, LastLoudShape::Silent, LastLoudImplementation::Rposition))]
#[bench::case_last_native_128(args = (128_usize, LastLoudShape::Last, LastLoudImplementation::Native))]
#[bench::case_last_rposition_128(args = (128_usize, LastLoudShape::Last, LastLoudImplementation::Rposition))]
#[bench::case_silent_native_512(args = (512_usize, LastLoudShape::Silent, LastLoudImplementation::Native))]
#[bench::case_silent_rposition_512(args = (512_usize, LastLoudShape::Silent, LastLoudImplementation::Rposition))]
#[bench::case_last_native_512(args = (512_usize, LastLoudShape::Last, LastLoudImplementation::Native))]
#[bench::case_last_rposition_512(args = (512_usize, LastLoudShape::Last, LastLoudImplementation::Rposition))]
#[bench::case_silent_native_4096(args = (4096_usize, LastLoudShape::Silent, LastLoudImplementation::Native))]
#[bench::case_silent_rposition_4096(args = (4096_usize, LastLoudShape::Silent, LastLoudImplementation::Rposition))]
#[bench::case_last_native_4096(args = (4096_usize, LastLoudShape::Last, LastLoudImplementation::Native))]
#[bench::case_last_rposition_4096(args = (4096_usize, LastLoudShape::Last, LastLoudImplementation::Rposition))]
fn native_last_loud(prepared: LastLoudPrepared) -> LastLoudPrepared {
    std::hint::black_box((prepared.search)(
        std::hint::black_box(&prepared.samples),
        std::hint::black_box(1e-6),
    ));
    prepared
}

#[derive(Clone, Copy)]
enum SvfFixedMode {
    Fixed,
    PerFrame,
    Scalar,
}

struct SvfFixedPrepared {
    frames: usize,
    response: Response,
    mode: SvfFixedMode,
    filter: StateVariableFilter<2>,
    planes: [Vec<f32>; 2],
    cutoffs: Vec<f64>,
    design: SvfDesign,
    state: [[f64; 2]; 2],
    coefficients: [f64; 4],
}

fn prepare_svf_fixed(
    frames: usize,
    response: Response,
    mode: SvfFixedMode,
) -> SvfFixedPrepared {
    let rate = NonZeroU32::new(48_000).expect("rate");
    let samples: Vec<_> = tone(frames)
        .into_iter()
        .map(|sample| 0.25 * sample)
        .collect();
    let planes = [samples.clone(), samples];
    let cutoffs = vec![4000.0; frames];
    let design = SvfDesign::new(rate, 4000.0, FRAC_1_SQRT_2, response).expect("design");
    let gain = (std::f64::consts::PI * 4000.0 / f64::from(rate.get())).tan();
    let damping = FRAC_1_SQRT_2.recip();
    let a1 = (1.0 + gain * (gain + damping)).recip();
    let a2 = gain * a1;
    let coefficients = [a1, a2, gain * a2, damping];
    SvfFixedPrepared {
        frames,
        response,
        mode,
        filter: StateVariableFilter::<2>::new(),
        planes,
        cutoffs,
        design,
        state: [[0.0_f64; 2]; 2],
        coefficients,
    }
}

#[metabench::benchmark(KERNEL_SVF_FIXED, "svf_fixed", "case", gungraun_setup = prepare_svf_fixed)]
#[bench::case_2ch_lowpass_fixed_128(args = (128_usize, Response::LowPass, SvfFixedMode::Fixed))]
#[bench::case_2ch_lowpass_per_frame_128(args = (128_usize, Response::LowPass, SvfFixedMode::PerFrame))]
#[bench::case_2ch_lowpass_scalar_128(args = (128_usize, Response::LowPass, SvfFixedMode::Scalar))]
#[bench::case_2ch_highpass_fixed_128(args = (128_usize, Response::HighPass, SvfFixedMode::Fixed))]
#[bench::case_2ch_highpass_per_frame_128(args = (128_usize, Response::HighPass, SvfFixedMode::PerFrame))]
#[bench::case_2ch_highpass_scalar_128(args = (128_usize, Response::HighPass, SvfFixedMode::Scalar))]
#[bench::case_2ch_lowpass_fixed_512(args = (512_usize, Response::LowPass, SvfFixedMode::Fixed))]
#[bench::case_2ch_lowpass_per_frame_512(args = (512_usize, Response::LowPass, SvfFixedMode::PerFrame))]
#[bench::case_2ch_lowpass_scalar_512(args = (512_usize, Response::LowPass, SvfFixedMode::Scalar))]
#[bench::case_2ch_highpass_fixed_512(args = (512_usize, Response::HighPass, SvfFixedMode::Fixed))]
#[bench::case_2ch_highpass_per_frame_512(args = (512_usize, Response::HighPass, SvfFixedMode::PerFrame))]
#[bench::case_2ch_highpass_scalar_512(args = (512_usize, Response::HighPass, SvfFixedMode::Scalar))]
fn native_svf_fixed(mut prepared: SvfFixedPrepared) -> SvfFixedPrepared {
    let rate = NonZeroU32::new(48_000).expect("rate");
    match prepared.mode {
        SvfFixedMode::Fixed => prepared
            .filter
            .process_fixed(
                std::hint::black_box(&mut prepared.planes),
                0..prepared.frames,
                &prepared.design,
            )
            .expect("fixed"),
        SvfFixedMode::PerFrame => prepared
            .filter
            .process(
                std::hint::black_box(&mut prepared.planes),
                0..prepared.frames,
                &prepared.cutoffs,
                rate,
                FRAC_1_SQRT_2,
                prepared.response,
            )
            .expect("dynamic"),
        SvfFixedMode::Scalar => scalar_svf_fixed(
            &mut prepared.planes,
            &mut prepared.state,
            &prepared.coefficients,
            prepared.response,
        ),
    }
    std::hint::black_box(&prepared.planes);
    prepared
}

#[derive(Clone, Copy)]
struct CrossfadeCase {
    channels: NonZeroUsize,
    frames: usize,
    tail: usize,
    chunk: usize,
    input_sample: f32,
}

impl CrossfadeCase {
    const fn new(
        channels: NonZeroUsize,
        frames: usize,
        tail: usize,
        chunk: usize,
        input_sample: f32,
    ) -> Self {
        Self {
            channels,
            frames,
            tail,
            chunk,
            input_sample,
        }
    }
}

struct CrossfadePrepared {
    old: Vec<f32>,
    output: Vec<f32>,
    channels: NonZeroUsize,
    frames: usize,
    chunk: usize,
}

fn apply_crossfade<F>(
    old: &[f32],
    output: &mut [f32],
    channels: NonZeroUsize,
    frames: usize,
    chunk: usize,
    kernel: F,
) -> usize
where
    F: Fn(&[f32], &mut [f32], NonZeroUsize, usize, usize) -> usize,
{
    let mut processed = 0;
    for (outgoing, incoming) in old
        .chunks(chunk * channels.get())
        .zip(output.chunks_mut(chunk * channels.get()))
    {
        processed += kernel(outgoing, incoming, channels, processed, frames);
    }
    processed
}

fn prepare_crossfade(case: CrossfadeCase, kernel: Crossfade) -> CrossfadePrepared {
    let old_sample = 0.314_159_f32;
    let old = vec![old_sample; case.frames * case.channels.get() + case.tail];
    let mut expected = vec![case.input_sample; old.len()];
    assert_eq!(
        previous_audio_crossfade(&old, &mut expected, case.channels, 0, case.frames),
        case.frames
    );
    let mut actual = vec![case.input_sample; expected.len()];
    assert_eq!(
        apply_crossfade(
            &old,
            &mut actual,
            case.channels,
            case.frames,
            case.chunk,
            kernel,
        ),
        case.frames
    );
    assert!(
        actual
            .iter()
            .zip(&expected)
            .all(|(left, right)| left.to_bits() == right.to_bits())
    );
    CrossfadePrepared {
        old,
        output: vec![case.input_sample; expected.len()],
        channels: case.channels,
        frames: case.frames,
        chunk: case.chunk,
    }
}

fn prepare_linear(case: CrossfadeCase) -> CrossfadePrepared {
    prepare_crossfade(case, kithara_dsp::crossfade_linear)
}

#[metabench::benchmark(KERNEL_CROSSFADE_LINEAR, "crossfade_linear", "case", gungraun_setup = prepare_linear)]
#[bench::case_1ch_64(args = (CrossfadeCase::new(NonZeroUsize::MIN, 64, 0, 64, -0.271828)))]
#[bench::case_2ch_64(args = (CrossfadeCase::new(consts::TWO, 64, 0, 64, -0.271828)))]
#[bench::case_6ch_64(args = (CrossfadeCase::new(consts::SIX, 64, 0, 64, -0.271828)))]
#[bench::case_1ch_128(args = (CrossfadeCase::new(NonZeroUsize::MIN, 128, 0, 128, -0.271828)))]
#[bench::case_2ch_128(args = (CrossfadeCase::new(consts::TWO, 128, 0, 128, -0.271828)))]
#[bench::case_6ch_128(args = (CrossfadeCase::new(consts::SIX, 128, 0, 128, -0.271828)))]
#[bench::case_1ch_256(args = (CrossfadeCase::new(NonZeroUsize::MIN, 256, 0, 256, -0.271828)))]
#[bench::case_2ch_256(args = (CrossfadeCase::new(consts::TWO, 256, 0, 256, -0.271828)))]
#[bench::case_6ch_256(args = (CrossfadeCase::new(consts::SIX, 256, 0, 256, -0.271828)))]
#[bench::case_1ch_512(args = (CrossfadeCase::new(NonZeroUsize::MIN, 512, 0, 512, -0.271828)))]
#[bench::case_2ch_512(args = (CrossfadeCase::new(consts::TWO, 512, 0, 512, -0.271828)))]
#[bench::case_6ch_512(args = (CrossfadeCase::new(consts::SIX, 512, 0, 512, -0.271828)))]
#[bench::case_1ch_1024(args = (CrossfadeCase::new(NonZeroUsize::MIN, 1024, 0, 1024, -0.271828)))]
#[bench::case_2ch_1024(args = (CrossfadeCase::new(consts::TWO, 1024, 0, 1024, -0.271828)))]
#[bench::case_6ch_1024(args = (CrossfadeCase::new(consts::SIX, 1024, 0, 1024, -0.271828)))]
#[bench::case_1ch_4096(args = (CrossfadeCase::new(NonZeroUsize::MIN, 4096, 0, 4096, -0.271828)))]
#[bench::case_2ch_4096(args = (CrossfadeCase::new(consts::TWO, 4096, 0, 4096, -0.271828)))]
#[bench::case_6ch_4096(args = (CrossfadeCase::new(consts::SIX, 4096, 0, 4096, -0.271828)))]
#[bench::case_2ch_7(args = (CrossfadeCase::new(consts::TWO, 7, 0, 7, -0.271828)))]
#[bench::case_6ch_127(args = (CrossfadeCase::new(consts::SIX, 127, 0, 127, -0.271828)))]
#[bench::case_2ch_127_tail(args = (CrossfadeCase::new(consts::TWO, 127, 1, 127, -0.271828)))]
#[bench::case_2ch_4096_chunk128(args = (CrossfadeCase::new(consts::TWO, 4096, 0, 128, -0.271828)))]
#[bench::case_2ch_7_identity(args = (CrossfadeCase::new(consts::TWO, 7, 0, 7, 0.314159)))]
fn native_crossfade_linear(mut prepared: CrossfadePrepared) -> CrossfadePrepared {
    std::hint::black_box(apply_crossfade(
        std::hint::black_box(&prepared.old),
        std::hint::black_box(&mut prepared.output),
        prepared.channels,
        prepared.frames,
        prepared.chunk,
        kithara_dsp::crossfade_linear,
    ));
    prepared
}

fn prepare_previous(case: CrossfadeCase) -> CrossfadePrepared {
    prepare_crossfade(case, previous_audio_crossfade)
}

#[metabench::benchmark(KERNEL_CROSSFADE_PREVIOUS, "crossfade_previous_audio", "case", gungraun_setup = prepare_previous)]
#[bench::case_1ch_64(args = (CrossfadeCase::new(NonZeroUsize::MIN, 64, 0, 64, -0.271828)))]
#[bench::case_2ch_64(args = (CrossfadeCase::new(consts::TWO, 64, 0, 64, -0.271828)))]
#[bench::case_6ch_64(args = (CrossfadeCase::new(consts::SIX, 64, 0, 64, -0.271828)))]
#[bench::case_1ch_128(args = (CrossfadeCase::new(NonZeroUsize::MIN, 128, 0, 128, -0.271828)))]
#[bench::case_2ch_128(args = (CrossfadeCase::new(consts::TWO, 128, 0, 128, -0.271828)))]
#[bench::case_6ch_128(args = (CrossfadeCase::new(consts::SIX, 128, 0, 128, -0.271828)))]
#[bench::case_1ch_256(args = (CrossfadeCase::new(NonZeroUsize::MIN, 256, 0, 256, -0.271828)))]
#[bench::case_2ch_256(args = (CrossfadeCase::new(consts::TWO, 256, 0, 256, -0.271828)))]
#[bench::case_6ch_256(args = (CrossfadeCase::new(consts::SIX, 256, 0, 256, -0.271828)))]
#[bench::case_1ch_512(args = (CrossfadeCase::new(NonZeroUsize::MIN, 512, 0, 512, -0.271828)))]
#[bench::case_2ch_512(args = (CrossfadeCase::new(consts::TWO, 512, 0, 512, -0.271828)))]
#[bench::case_6ch_512(args = (CrossfadeCase::new(consts::SIX, 512, 0, 512, -0.271828)))]
#[bench::case_1ch_1024(args = (CrossfadeCase::new(NonZeroUsize::MIN, 1024, 0, 1024, -0.271828)))]
#[bench::case_2ch_1024(args = (CrossfadeCase::new(consts::TWO, 1024, 0, 1024, -0.271828)))]
#[bench::case_6ch_1024(args = (CrossfadeCase::new(consts::SIX, 1024, 0, 1024, -0.271828)))]
#[bench::case_1ch_4096(args = (CrossfadeCase::new(NonZeroUsize::MIN, 4096, 0, 4096, -0.271828)))]
#[bench::case_2ch_4096(args = (CrossfadeCase::new(consts::TWO, 4096, 0, 4096, -0.271828)))]
#[bench::case_6ch_4096(args = (CrossfadeCase::new(consts::SIX, 4096, 0, 4096, -0.271828)))]
#[bench::case_2ch_7(args = (CrossfadeCase::new(consts::TWO, 7, 0, 7, -0.271828)))]
#[bench::case_6ch_127(args = (CrossfadeCase::new(consts::SIX, 127, 0, 127, -0.271828)))]
#[bench::case_2ch_127_tail(args = (CrossfadeCase::new(consts::TWO, 127, 1, 127, -0.271828)))]
#[bench::case_2ch_4096_chunk128(args = (CrossfadeCase::new(consts::TWO, 4096, 0, 128, -0.271828)))]
#[bench::case_2ch_7_identity(args = (CrossfadeCase::new(consts::TWO, 7, 0, 7, 0.314159)))]
fn native_crossfade_previous(mut prepared: CrossfadePrepared) -> CrossfadePrepared {
    std::hint::black_box(apply_crossfade(
        std::hint::black_box(&prepared.old),
        std::hint::black_box(&mut prepared.output),
        prepared.channels,
        prepared.frames,
        prepared.chunk,
        previous_audio_crossfade,
    ));
    prepared
}

#[derive(Clone, Copy)]
enum LayoutOperation {
    FiInterleave,
    FiDeinterleave,
    Interleave,
    Deinterleave,
    Sanitize,
}

struct LayoutPrepared {
    frames: usize,
    channels: NonZeroUsize,
    operation: LayoutOperation,
    planes: Vec<Vec<f32>>,
    planar: Vec<f32>,
    stride: NonZeroUsize,
    restored: Vec<Vec<f32>>,
    interleaved: Vec<f32>,
    noisy: Vec<f32>,
}

fn prepare_layout(
    frames: usize,
    channels: NonZeroUsize,
    operation: LayoutOperation,
) -> LayoutPrepared {
    let planes = vec![vec![0.25_f32; frames]; consts::SIX.get()];
    LayoutPrepared {
        frames,
        channels,
        operation,
        planar: planes.concat(),
        stride: NonZeroUsize::new(frames).expect("stride"),
        restored: planes.clone(),
        interleaved: vec![0.0_f32; channels.get() * frames],
        noisy: vec![f32::from_bits(1); frames],
        planes,
    }
}

#[metabench::benchmark(KERNEL_LAYOUT, "layout", "case", gungraun_setup = prepare_layout)]
#[bench::case_fi_interleave_2ch_64(args = (64_usize, consts::TWO, LayoutOperation::FiInterleave))]
#[bench::case_fi_deinterleave_2ch_64(args = (64_usize, consts::TWO, LayoutOperation::FiDeinterleave))]
#[bench::case_interleave_2ch_64(args = (64_usize, consts::TWO, LayoutOperation::Interleave))]
#[bench::case_deinterleave_2ch_64(args = (64_usize, consts::TWO, LayoutOperation::Deinterleave))]
#[bench::case_fi_interleave_6ch_64(args = (64_usize, consts::SIX, LayoutOperation::FiInterleave))]
#[bench::case_fi_deinterleave_6ch_64(args = (64_usize, consts::SIX, LayoutOperation::FiDeinterleave))]
#[bench::case_interleave_6ch_64(args = (64_usize, consts::SIX, LayoutOperation::Interleave))]
#[bench::case_deinterleave_6ch_64(args = (64_usize, consts::SIX, LayoutOperation::Deinterleave))]
#[bench::case_sanitize_64(args = (64_usize, consts::TWO, LayoutOperation::Sanitize))]
#[bench::case_fi_interleave_2ch_128(args = (128_usize, consts::TWO, LayoutOperation::FiInterleave))]
#[bench::case_fi_deinterleave_2ch_128(args = (128_usize, consts::TWO, LayoutOperation::FiDeinterleave))]
#[bench::case_interleave_2ch_128(args = (128_usize, consts::TWO, LayoutOperation::Interleave))]
#[bench::case_deinterleave_2ch_128(args = (128_usize, consts::TWO, LayoutOperation::Deinterleave))]
#[bench::case_fi_interleave_6ch_128(args = (128_usize, consts::SIX, LayoutOperation::FiInterleave))]
#[bench::case_fi_deinterleave_6ch_128(args = (128_usize, consts::SIX, LayoutOperation::FiDeinterleave))]
#[bench::case_interleave_6ch_128(args = (128_usize, consts::SIX, LayoutOperation::Interleave))]
#[bench::case_deinterleave_6ch_128(args = (128_usize, consts::SIX, LayoutOperation::Deinterleave))]
#[bench::case_sanitize_128(args = (128_usize, consts::TWO, LayoutOperation::Sanitize))]
#[bench::case_fi_interleave_2ch_256(args = (256_usize, consts::TWO, LayoutOperation::FiInterleave))]
#[bench::case_fi_deinterleave_2ch_256(args = (256_usize, consts::TWO, LayoutOperation::FiDeinterleave))]
#[bench::case_interleave_2ch_256(args = (256_usize, consts::TWO, LayoutOperation::Interleave))]
#[bench::case_deinterleave_2ch_256(args = (256_usize, consts::TWO, LayoutOperation::Deinterleave))]
#[bench::case_fi_interleave_6ch_256(args = (256_usize, consts::SIX, LayoutOperation::FiInterleave))]
#[bench::case_fi_deinterleave_6ch_256(args = (256_usize, consts::SIX, LayoutOperation::FiDeinterleave))]
#[bench::case_interleave_6ch_256(args = (256_usize, consts::SIX, LayoutOperation::Interleave))]
#[bench::case_deinterleave_6ch_256(args = (256_usize, consts::SIX, LayoutOperation::Deinterleave))]
#[bench::case_sanitize_256(args = (256_usize, consts::TWO, LayoutOperation::Sanitize))]
#[bench::case_fi_interleave_2ch_512(args = (512_usize, consts::TWO, LayoutOperation::FiInterleave))]
#[bench::case_fi_deinterleave_2ch_512(args = (512_usize, consts::TWO, LayoutOperation::FiDeinterleave))]
#[bench::case_interleave_2ch_512(args = (512_usize, consts::TWO, LayoutOperation::Interleave))]
#[bench::case_deinterleave_2ch_512(args = (512_usize, consts::TWO, LayoutOperation::Deinterleave))]
#[bench::case_fi_interleave_6ch_512(args = (512_usize, consts::SIX, LayoutOperation::FiInterleave))]
#[bench::case_fi_deinterleave_6ch_512(args = (512_usize, consts::SIX, LayoutOperation::FiDeinterleave))]
#[bench::case_interleave_6ch_512(args = (512_usize, consts::SIX, LayoutOperation::Interleave))]
#[bench::case_deinterleave_6ch_512(args = (512_usize, consts::SIX, LayoutOperation::Deinterleave))]
#[bench::case_sanitize_512(args = (512_usize, consts::TWO, LayoutOperation::Sanitize))]
#[bench::case_fi_interleave_2ch_1024(args = (1024_usize, consts::TWO, LayoutOperation::FiInterleave))]
#[bench::case_fi_deinterleave_2ch_1024(args = (1024_usize, consts::TWO, LayoutOperation::FiDeinterleave))]
#[bench::case_interleave_2ch_1024(args = (1024_usize, consts::TWO, LayoutOperation::Interleave))]
#[bench::case_deinterleave_2ch_1024(args = (1024_usize, consts::TWO, LayoutOperation::Deinterleave))]
#[bench::case_fi_interleave_6ch_1024(args = (1024_usize, consts::SIX, LayoutOperation::FiInterleave))]
#[bench::case_fi_deinterleave_6ch_1024(args = (1024_usize, consts::SIX, LayoutOperation::FiDeinterleave))]
#[bench::case_interleave_6ch_1024(args = (1024_usize, consts::SIX, LayoutOperation::Interleave))]
#[bench::case_deinterleave_6ch_1024(args = (1024_usize, consts::SIX, LayoutOperation::Deinterleave))]
#[bench::case_sanitize_1024(args = (1024_usize, consts::TWO, LayoutOperation::Sanitize))]
#[bench::case_fi_interleave_2ch_4096(args = (4096_usize, consts::TWO, LayoutOperation::FiInterleave))]
#[bench::case_fi_deinterleave_2ch_4096(args = (4096_usize, consts::TWO, LayoutOperation::FiDeinterleave))]
#[bench::case_interleave_2ch_4096(args = (4096_usize, consts::TWO, LayoutOperation::Interleave))]
#[bench::case_deinterleave_2ch_4096(args = (4096_usize, consts::TWO, LayoutOperation::Deinterleave))]
#[bench::case_fi_interleave_6ch_4096(args = (4096_usize, consts::SIX, LayoutOperation::FiInterleave))]
#[bench::case_fi_deinterleave_6ch_4096(args = (4096_usize, consts::SIX, LayoutOperation::FiDeinterleave))]
#[bench::case_interleave_6ch_4096(args = (4096_usize, consts::SIX, LayoutOperation::Interleave))]
#[bench::case_deinterleave_6ch_4096(args = (4096_usize, consts::SIX, LayoutOperation::Deinterleave))]
#[bench::case_sanitize_4096(args = (4096_usize, consts::TWO, LayoutOperation::Sanitize))]
fn native_layout(mut prepared: LayoutPrepared) -> LayoutPrepared {
    match prepared.operation {
        LayoutOperation::FiInterleave => fast_interleave::interleave_variable(
            std::hint::black_box(&prepared.planes[..prepared.channels.get()]),
            0..prepared.frames,
            &mut prepared.interleaved,
            prepared.channels,
        ),
        LayoutOperation::FiDeinterleave => fast_interleave::deinterleave_variable(
            std::hint::black_box(&prepared.interleaved),
            prepared.channels,
            &mut prepared.restored,
            0..prepared.frames,
        ),
        LayoutOperation::Interleave => kithara_dsp::interleave_channel_major(
            std::hint::black_box(&prepared.planar[..prepared.channels.get() * prepared.frames]),
            prepared.stride,
            0..prepared.frames,
            &mut prepared.interleaved,
            prepared.channels,
        ),
        LayoutOperation::Deinterleave => kithara_dsp::deinterleave_variable(
            std::hint::black_box(&prepared.interleaved),
            prepared.channels,
            &mut prepared.restored,
            0..prepared.frames,
        ),
        LayoutOperation::Sanitize => kithara_dsp::sanitize(std::hint::black_box(&mut prepared.noisy)),
    }
    prepared
}

struct VectorPrepared {
    samples: Vec<f32>,
}

fn prepare_vector(size: usize) -> VectorPrepared {
    VectorPrepared { samples: tone(size) }
}

#[metabench::benchmark(KERNEL_VECTOR, "vector", "case", gungraun_setup = prepare_vector)]
#[bench::case_sum_squares_64(64_usize)]
#[bench::case_sum_squares_128(128_usize)]
#[bench::case_sum_squares_256(256_usize)]
#[bench::case_sum_squares_512(512_usize)]
#[bench::case_sum_squares_1024(1024_usize)]
#[bench::case_sum_squares_4096(4096_usize)]
fn native_sum_squares(prepared: VectorPrepared) -> VectorPrepared {
    let _ = kithara_dsp::sum_squares(std::hint::black_box(&prepared.samples));
    prepared
}

#[derive(Clone, Copy)]
enum CurveImplementation {
    Native,
    Scalar,
    Simd,
    Entry,
}

struct CurvePrepared {
    shape: CurveShape,
    values: Vec<f64>,
    implementation: CurveImplementation,
}

fn prepare_curve(
    frames: usize,
    shape: CurveShape,
    implementation: CurveImplementation,
) -> CurvePrepared {
    let values = (0..frames as u32)
        .map(|frame| f64::from(frame) / f64::from(frames as u32 - 1))
        .collect();
    CurvePrepared {
        shape,
        values,
        implementation,
    }
}

fn prepare_curve_hold(frames: usize, implementation: CurveImplementation) -> CurvePrepared {
    prepare_curve(frames, CurveShape::HOLD, implementation)
}

#[metabench::benchmark(KERNEL_CURVE_HOLD, "curve_hold", "case", gungraun_setup = prepare_curve_hold)]
#[bench::case_native_128(args = (128_usize, CurveImplementation::Native))]
#[bench::case_scalar_128(args = (128_usize, CurveImplementation::Scalar))]
#[bench::case_simd_128(args = (128_usize, CurveImplementation::Simd))]
#[bench::case_entry_128(args = (128_usize, CurveImplementation::Entry))]
#[bench::case_native_512(args = (512_usize, CurveImplementation::Native))]
#[bench::case_scalar_512(args = (512_usize, CurveImplementation::Scalar))]
#[bench::case_simd_512(args = (512_usize, CurveImplementation::Simd))]
#[bench::case_entry_512(args = (512_usize, CurveImplementation::Entry))]
fn native_curve_hold(mut prepared: CurvePrepared) -> CurvePrepared {
    match prepared.implementation {
        CurveImplementation::Native => prepared.shape.process(
            std::hint::black_box(&mut prepared.values),
            std::hint::black_box([0.75, 0.25]),
        ),
        CurveImplementation::Scalar => scalar_curve(
            "hold",
            std::hint::black_box(&mut prepared.values),
            std::hint::black_box([0.75, 0.25]),
        ),
        CurveImplementation::Simd => {
            let kernel = kithara_dsp::routes::parameter_hold::routes()
                .next()
                .expect("curve route")
                .1;
            kernel(
                std::hint::black_box(&mut prepared.values),
                std::hint::black_box([0.75, 0.25]),
            );
        }
        CurveImplementation::Entry => {
            let kernel = kithara_dsp::routes::parameter_hold::routes()
                .nth(1)
                .expect("curve route")
                .1;
            kernel(
                std::hint::black_box(&mut prepared.values),
                std::hint::black_box([0.75, 0.25]),
            );
        }
    }
    prepared
}

fn prepare_curve_linear(frames: usize, implementation: CurveImplementation) -> CurvePrepared {
    prepare_curve(frames, CurveShape::LINEAR, implementation)
}

#[metabench::benchmark(KERNEL_CURVE_LINEAR, "curve_linear", "case", gungraun_setup = prepare_curve_linear)]
#[bench::case_native_128(args = (128_usize, CurveImplementation::Native))]
#[bench::case_scalar_128(args = (128_usize, CurveImplementation::Scalar))]
#[bench::case_simd_128(args = (128_usize, CurveImplementation::Simd))]
#[bench::case_entry_128(args = (128_usize, CurveImplementation::Entry))]
#[bench::case_native_512(args = (512_usize, CurveImplementation::Native))]
#[bench::case_scalar_512(args = (512_usize, CurveImplementation::Scalar))]
#[bench::case_simd_512(args = (512_usize, CurveImplementation::Simd))]
#[bench::case_entry_512(args = (512_usize, CurveImplementation::Entry))]
fn native_curve_linear(mut prepared: CurvePrepared) -> CurvePrepared {
    match prepared.implementation {
        CurveImplementation::Native => prepared.shape.process(
            std::hint::black_box(&mut prepared.values),
            std::hint::black_box([0.75, 0.25]),
        ),
        CurveImplementation::Scalar => scalar_curve(
            "linear",
            std::hint::black_box(&mut prepared.values),
            std::hint::black_box([0.75, 0.25]),
        ),
        CurveImplementation::Simd => {
            let kernel = kithara_dsp::routes::parameter_linear::routes()
                .next()
                .expect("curve route")
                .1;
            kernel(
                std::hint::black_box(&mut prepared.values),
                std::hint::black_box([0.75, 0.25]),
            );
        }
        CurveImplementation::Entry => {
            let kernel = kithara_dsp::routes::parameter_linear::routes()
                .nth(1)
                .expect("curve route")
                .1;
            kernel(
                std::hint::black_box(&mut prepared.values),
                std::hint::black_box([0.75, 0.25]),
            );
        }
    }
    prepared
}

fn prepare_curve_cubic(frames: usize, implementation: CurveImplementation) -> CurvePrepared {
    prepare_curve(
        frames,
        CurveShape::cubic(0.2, 0.6).expect("ordered handles"),
        implementation,
    )
}

#[metabench::benchmark(KERNEL_CURVE_CUBIC, "curve_cubic", "case", gungraun_setup = prepare_curve_cubic)]
#[bench::case_native_128(args = (128_usize, CurveImplementation::Native))]
#[bench::case_scalar_128(args = (128_usize, CurveImplementation::Scalar))]
#[bench::case_native_512(args = (512_usize, CurveImplementation::Native))]
#[bench::case_scalar_512(args = (512_usize, CurveImplementation::Scalar))]
fn native_curve_cubic(mut prepared: CurvePrepared) -> CurvePrepared {
    match prepared.implementation {
        CurveImplementation::Native => prepared.shape.process(
            std::hint::black_box(&mut prepared.values),
            std::hint::black_box([0.75, 0.25]),
        ),
        CurveImplementation::Scalar => scalar_curve(
            "cubic",
            std::hint::black_box(&mut prepared.values),
            std::hint::black_box([0.75, 0.25]),
        ),
        _ => unreachable!("cubic has no routed form"),
    }
    prepared
}

#[derive(Clone, Copy)]
enum RatioImplementation {
    Simd,
    Scalar,
    Entry,
    Native,
}

struct RatioRampPrepared {
    output: Vec<f64>,
    kernel: kithara_dsp::routes::ratio_ramp::Kernel,
}

fn prepare_ratio_ramp(
    frames: usize,
    implementation: RatioImplementation,
) -> RatioRampPrepared {
    let native: kithara_dsp::routes::ratio_ramp::Kernel = kithara_dsp::ratio_ramp;
    let kernel = match implementation {
        RatioImplementation::Simd => kithara_dsp::routes::ratio_ramp::routes()
            .next()
            .expect("ratio route")
            .1,
        RatioImplementation::Scalar => kithara_dsp::routes::ratio_ramp::routes()
            .nth(1)
            .expect("ratio route")
            .1,
        RatioImplementation::Entry => kithara_dsp::routes::ratio_ramp::routes()
            .nth(2)
            .expect("ratio route")
            .1,
        RatioImplementation::Native => native,
    };
    RatioRampPrepared {
        output: vec![0.0; frames],
        kernel,
    }
}

#[metabench::benchmark(KERNEL_RATIO_RAMP, "ratio_ramp", "case", gungraun_setup = prepare_ratio_ramp)]
#[bench::case_simd_128(args = (128_usize, RatioImplementation::Simd))]
#[bench::case_scalar_128(args = (128_usize, RatioImplementation::Scalar))]
#[bench::case_entry_128(args = (128_usize, RatioImplementation::Entry))]
#[bench::case_native_128(args = (128_usize, RatioImplementation::Native))]
#[bench::case_simd_512(args = (512_usize, RatioImplementation::Simd))]
#[bench::case_scalar_512(args = (512_usize, RatioImplementation::Scalar))]
#[bench::case_entry_512(args = (512_usize, RatioImplementation::Entry))]
#[bench::case_native_512(args = (512_usize, RatioImplementation::Native))]
fn native_ratio_ramp(mut prepared: RatioRampPrepared) -> RatioRampPrepared {
    (prepared.kernel)(
        std::hint::black_box(&mut prepared.output),
        std::hint::black_box((1_u64 << 25) + 3),
        std::hint::black_box(NonZeroU64::new(997).expect("positive")),
    );
    prepared
}

#[derive(Clone, Copy)]
enum NativeOrScalar {
    Native,
    Scalar,
}

struct NarrowPrepared {
    input: Vec<f64>,
    output: Vec<f32>,
    implementation: NativeOrScalar,
}

fn prepare_narrow(frames: usize, implementation: NativeOrScalar) -> NarrowPrepared {
    NarrowPrepared {
        input: tone(frames).into_iter().map(f64::from).collect(),
        output: vec![0.0; frames],
        implementation,
    }
}

#[metabench::benchmark(KERNEL_NARROW, "narrow_f64", "case", gungraun_setup = prepare_narrow)]
#[bench::case_native_128(args = (128_usize, NativeOrScalar::Native))]
#[bench::case_scalar_128(args = (128_usize, NativeOrScalar::Scalar))]
#[bench::case_native_512(args = (512_usize, NativeOrScalar::Native))]
#[bench::case_scalar_512(args = (512_usize, NativeOrScalar::Scalar))]
fn native_narrow_f64(mut prepared: NarrowPrepared) -> NarrowPrepared {
    match prepared.implementation {
        NativeOrScalar::Native => {
            kithara_dsp::narrow_f64(&prepared.input, &mut prepared.output);
        }
        NativeOrScalar::Scalar => {
            for (value, input) in prepared.output.iter_mut().zip(&prepared.input) {
                *value = input.as_();
            }
        }
    }
    prepared
}

struct MultiplySquaredPrepared {
    output: Vec<f32>,
    square: Vec<f32>,
    gains: Vec<f32>,
    implementation: NativeOrScalar,
}

fn prepare_multiply_squared(
    frames: usize, implementation: NativeOrScalar,
) -> MultiplySquaredPrepared {
    MultiplySquaredPrepared {
        output: tone(frames),
        square: vec![0.7; frames],
        gains: vec![0.3; frames],
        implementation,
    }
}

#[metabench::benchmark(KERNEL_MULTIPLY_SQUARED, "multiply_squared_in_place", "case", gungraun_setup = prepare_multiply_squared)]
#[bench::case_native_128(args = (128_usize, NativeOrScalar::Native))]
#[bench::case_scalar_128(args = (128_usize, NativeOrScalar::Scalar))]
#[bench::case_native_512(args = (512_usize, NativeOrScalar::Native))]
#[bench::case_scalar_512(args = (512_usize, NativeOrScalar::Scalar))]
fn native_multiply_squared(mut prepared: MultiplySquaredPrepared) -> MultiplySquaredPrepared {
    match prepared.implementation {
        NativeOrScalar::Native => {
            kithara_dsp::multiply_squared_in_place(
                &mut prepared.output,
                &prepared.square,
                &prepared.gains,
            );
        }
        NativeOrScalar::Scalar => {
            for ((value, square), gain) in prepared
                .output
                .iter_mut()
                .zip(&prepared.square)
                .zip(&prepared.gains)
            {
                *value = (*value * (square * square)) * gain;
            }
        }
    }
    prepared
}

#[derive(Clone, Copy)]
enum RouteImplementation {
    Simd,
    Entry,
}

struct RampPrepared {
    input: Vec<f32>,
    output: Vec<f32>,
    kernel: kithara_dsp::routes::ramp::Kernel,
}

fn prepare_ramp(frames: usize, implementation: RouteImplementation) -> RampPrepared {
    let route = match implementation {
        RouteImplementation::Simd => 0,
        RouteImplementation::Entry => 1,
    };
    let input = tone(frames);
    RampPrepared {
        output: input.clone(),
        input,
        kernel: kithara_dsp::routes::ramp::routes()
            .nth(route)
            .expect("route")
            .1,
    }
}

#[metabench::benchmark(KERNEL_RAMP, "ramp", "case", gungraun_setup = prepare_ramp)]
#[bench::case_simd_128(args = (128_usize, RouteImplementation::Simd))]
#[bench::case_entry_128(args = (128_usize, RouteImplementation::Entry))]
#[bench::case_simd_512(args = (512_usize, RouteImplementation::Simd))]
#[bench::case_entry_512(args = (512_usize, RouteImplementation::Entry))]
fn native_ramp(mut prepared: RampPrepared) -> RampPrepared {
    (prepared.kernel)(
        std::hint::black_box(&mut prepared.output),
        std::hint::black_box(0.25),
        std::hint::black_box(0.001),
    );
    std::hint::black_box(&prepared.input);
    prepared
}

struct DivPrepared {
    input: Vec<f32>,
    output: Vec<f32>,
    kernel: kithara_dsp::routes::div::Kernel,
}

fn prepare_div(frames: usize, implementation: RouteImplementation) -> DivPrepared {
    let route = match implementation {
        RouteImplementation::Simd => 0,
        RouteImplementation::Entry => 1,
    };
    let input = tone(frames);
    DivPrepared {
        output: input.clone(),
        input,
        kernel: kithara_dsp::routes::div::routes()
            .nth(route)
            .expect("route")
            .1,
    }
}

#[metabench::benchmark(KERNEL_DIV, "div", "case", gungraun_setup = prepare_div)]
#[bench::case_simd_128(args = (128_usize, RouteImplementation::Simd))]
#[bench::case_entry_128(args = (128_usize, RouteImplementation::Entry))]
#[bench::case_simd_512(args = (512_usize, RouteImplementation::Simd))]
#[bench::case_entry_512(args = (512_usize, RouteImplementation::Entry))]
fn native_div(mut prepared: DivPrepared) -> DivPrepared {
    (prepared.kernel)(
        std::hint::black_box(&mut prepared.output),
        std::hint::black_box(1.25),
    );
    std::hint::black_box(&prepared.input);
    prepared
}

struct MulAddPrepared {
    input: Vec<f32>,
    output: Vec<f32>,
    kernel: kithara_dsp::routes::mul_add::Kernel,
}

fn prepare_mul_add(frames: usize, implementation: RouteImplementation) -> MulAddPrepared {
    let route = match implementation {
        RouteImplementation::Simd => 0,
        RouteImplementation::Entry => 1,
    };
    let input = tone(frames);
    MulAddPrepared {
        output: vec![0.25; frames],
        input,
        kernel: kithara_dsp::routes::mul_add::routes()
            .nth(route)
            .expect("route")
            .1,
    }
}

#[metabench::benchmark(KERNEL_MUL_ADD, "mul_add", "case", gungraun_setup = prepare_mul_add)]
#[bench::case_simd_128(args = (128_usize, RouteImplementation::Simd))]
#[bench::case_entry_128(args = (128_usize, RouteImplementation::Entry))]
#[bench::case_simd_512(args = (512_usize, RouteImplementation::Simd))]
#[bench::case_entry_512(args = (512_usize, RouteImplementation::Entry))]
fn native_mul_add(mut prepared: MulAddPrepared) -> MulAddPrepared {
    (prepared.kernel)(
        std::hint::black_box(&prepared.input),
        std::hint::black_box(&prepared.input),
        std::hint::black_box(&mut prepared.output),
    );
    std::hint::black_box(&prepared.input);
    prepared
}

struct DownmixPrepared {
    interleaved: Vec<f32>,
    mono: Vec<f32>,
    channels: NonZeroUsize,
}

fn prepare_downmix(frames: usize, channels: NonZeroUsize) -> DownmixPrepared {
    DownmixPrepared {
        interleaved: tone(channels.get() * frames),
        mono: vec![0.0_f32; frames],
        channels,
    }
}

#[metabench::benchmark(KERNEL_DOWNMIX, "downmix", "case", gungraun_setup = prepare_downmix)]
#[bench::case_2ch_64(args = (64_usize, consts::TWO))]
#[bench::case_6ch_64(args = (64_usize, consts::SIX))]
#[bench::case_2ch_128(args = (128_usize, consts::TWO))]
#[bench::case_6ch_128(args = (128_usize, consts::SIX))]
#[bench::case_2ch_256(args = (256_usize, consts::TWO))]
#[bench::case_6ch_256(args = (256_usize, consts::SIX))]
#[bench::case_2ch_512(args = (512_usize, consts::TWO))]
#[bench::case_6ch_512(args = (512_usize, consts::SIX))]
#[bench::case_2ch_1024(args = (1024_usize, consts::TWO))]
#[bench::case_6ch_1024(args = (1024_usize, consts::SIX))]
#[bench::case_2ch_4096(args = (4096_usize, consts::TWO))]
#[bench::case_6ch_4096(args = (4096_usize, consts::SIX))]
fn native_downmix(mut prepared: DownmixPrepared) -> DownmixPrepared {
    let _ = kithara_dsp::downmix(
        std::hint::black_box(&prepared.interleaved),
        prepared.channels,
        &mut prepared.mono,
    );
    prepared
}

struct BiquadPrepared {
    filter: Biquad,
    planes: Vec<Vec<f32>>,
}

fn prepare_biquad(channels: usize, sections: usize) -> BiquadPrepared {
    let mut filter = Biquad::new(
        NonZeroUsize::new(channels).expect("channels"),
        NonZeroUsize::new(sections).expect("sections"),
    )
    .expect("filter");
    for section in 0..sections {
        filter.retune(section, low_pass()).expect("section");
    }
    BiquadPrepared {
        filter,
        planes: vec![vec![0.25_f32; 1024]; channels],
    }
}

#[metabench::benchmark(KERNEL_BIQUAD, "biquad", "case", gungraun_setup = prepare_biquad)]
#[bench::case_1ch_1(args = (1_usize, 1_usize))]
#[bench::case_1ch_4(args = (1_usize, 4_usize))]
#[bench::case_2ch_1(args = (2_usize, 1_usize))]
#[bench::case_2ch_4(args = (2_usize, 4_usize))]
#[bench::case_8ch_1(args = (8_usize, 1_usize))]
#[bench::case_8ch_4(args = (8_usize, 4_usize))]
fn native_biquad(mut prepared: BiquadPrepared) -> BiquadPrepared {
    prepared
        .filter
        .process(std::hint::black_box(&mut prepared.planes), 0..1024)
        .expect("process");
    prepared
}

#[derive(Clone, Copy)]
enum CrossoverInput {
    Signal,
    Silence,
    QuietNoise,
}

#[derive(Clone, Copy)]
enum CrossoverImplementation {
    Df1,
    Simd,
    Scalar,
    Entry,
}

struct CrossoverPrepared {
    baseline: Option<Frame>,
    svf: Option<Svf>,
    io: [Vec<f32>; 2],
    gains: Vec<Vec<f32>>,
    frames: usize,
    kernel: Option<SplitKernel>,
}

fn prepare_crossover(
    bands: usize,
    frames: usize,
    input_kind: CrossoverInput,
    implementation: CrossoverImplementation,
) -> CrossoverPrepared {
    let frequencies = frequencies(bands);
    let inputs = input_forms(frames);
    let input = match input_kind {
        CrossoverInput::Signal => inputs[0].1.clone(),
        CrossoverInput::Silence => inputs[1].1.clone(),
        CrossoverInput::QuietNoise => inputs[2].1.clone(),
    };
    let gains = gains(bands, frames, true);
    let route = match implementation {
        CrossoverImplementation::Df1 => None,
        CrossoverImplementation::Simd => Some(0),
        CrossoverImplementation::Scalar => Some(1),
        CrossoverImplementation::Entry => Some(2),
    };
    let kernel = route.map(|route| {
        kithara_dsp::routes::crossover::forms::<2, Vec<f32>>()[route].1
    });
    CrossoverPrepared {
        baseline: matches!(implementation, CrossoverImplementation::Df1)
            .then(|| Frame::new(&frequencies)),
        svf: (!matches!(implementation, CrossoverImplementation::Df1))
            .then(|| Svf::new(&frequencies, frames)),
        io: input,
        gains,
        frames,
        kernel,
    }
}

#[metabench::benchmark(KERNEL_CROSSOVER, "crossover_df1_svf", "case", gungraun_setup = prepare_crossover)]
#[bench::case_df1_3band_128(args = (3_usize, 128_usize, CrossoverInput::Signal, CrossoverImplementation::Df1))]
#[bench::case_simd_3band_128(args = (3_usize, 128_usize, CrossoverInput::Signal, CrossoverImplementation::Simd))]
#[bench::case_scalar_3band_128(args = (3_usize, 128_usize, CrossoverInput::Signal, CrossoverImplementation::Scalar))]
#[bench::case_entry_3band_128(args = (3_usize, 128_usize, CrossoverInput::Signal, CrossoverImplementation::Entry))]
#[bench::case_df1_silence_3band_128(args = (3_usize, 128_usize, CrossoverInput::Silence, CrossoverImplementation::Df1))]
#[bench::case_simd_silence_3band_128(args = (3_usize, 128_usize, CrossoverInput::Silence, CrossoverImplementation::Simd))]
#[bench::case_scalar_silence_3band_128(args = (3_usize, 128_usize, CrossoverInput::Silence, CrossoverImplementation::Scalar))]
#[bench::case_entry_silence_3band_128(args = (3_usize, 128_usize, CrossoverInput::Silence, CrossoverImplementation::Entry))]
#[bench::case_df1_quiet_noise_3band_128(args = (3_usize, 128_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Df1))]
#[bench::case_simd_quiet_noise_3band_128(args = (3_usize, 128_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Simd))]
#[bench::case_scalar_quiet_noise_3band_128(args = (3_usize, 128_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Scalar))]
#[bench::case_entry_quiet_noise_3band_128(args = (3_usize, 128_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Entry))]
#[bench::case_df1_3band_512(args = (3_usize, 512_usize, CrossoverInput::Signal, CrossoverImplementation::Df1))]
#[bench::case_simd_3band_512(args = (3_usize, 512_usize, CrossoverInput::Signal, CrossoverImplementation::Simd))]
#[bench::case_scalar_3band_512(args = (3_usize, 512_usize, CrossoverInput::Signal, CrossoverImplementation::Scalar))]
#[bench::case_entry_3band_512(args = (3_usize, 512_usize, CrossoverInput::Signal, CrossoverImplementation::Entry))]
#[bench::case_df1_silence_3band_512(args = (3_usize, 512_usize, CrossoverInput::Silence, CrossoverImplementation::Df1))]
#[bench::case_simd_silence_3band_512(args = (3_usize, 512_usize, CrossoverInput::Silence, CrossoverImplementation::Simd))]
#[bench::case_scalar_silence_3band_512(args = (3_usize, 512_usize, CrossoverInput::Silence, CrossoverImplementation::Scalar))]
#[bench::case_entry_silence_3band_512(args = (3_usize, 512_usize, CrossoverInput::Silence, CrossoverImplementation::Entry))]
#[bench::case_df1_quiet_noise_3band_512(args = (3_usize, 512_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Df1))]
#[bench::case_simd_quiet_noise_3band_512(args = (3_usize, 512_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Simd))]
#[bench::case_scalar_quiet_noise_3band_512(args = (3_usize, 512_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Scalar))]
#[bench::case_entry_quiet_noise_3band_512(args = (3_usize, 512_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Entry))]
#[bench::case_df1_10band_128(args = (10_usize, 128_usize, CrossoverInput::Signal, CrossoverImplementation::Df1))]
#[bench::case_simd_10band_128(args = (10_usize, 128_usize, CrossoverInput::Signal, CrossoverImplementation::Simd))]
#[bench::case_scalar_10band_128(args = (10_usize, 128_usize, CrossoverInput::Signal, CrossoverImplementation::Scalar))]
#[bench::case_entry_10band_128(args = (10_usize, 128_usize, CrossoverInput::Signal, CrossoverImplementation::Entry))]
#[bench::case_df1_silence_10band_128(args = (10_usize, 128_usize, CrossoverInput::Silence, CrossoverImplementation::Df1))]
#[bench::case_simd_silence_10band_128(args = (10_usize, 128_usize, CrossoverInput::Silence, CrossoverImplementation::Simd))]
#[bench::case_scalar_silence_10band_128(args = (10_usize, 128_usize, CrossoverInput::Silence, CrossoverImplementation::Scalar))]
#[bench::case_entry_silence_10band_128(args = (10_usize, 128_usize, CrossoverInput::Silence, CrossoverImplementation::Entry))]
#[bench::case_df1_quiet_noise_10band_128(args = (10_usize, 128_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Df1))]
#[bench::case_simd_quiet_noise_10band_128(args = (10_usize, 128_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Simd))]
#[bench::case_scalar_quiet_noise_10band_128(args = (10_usize, 128_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Scalar))]
#[bench::case_entry_quiet_noise_10band_128(args = (10_usize, 128_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Entry))]
#[bench::case_df1_10band_512(args = (10_usize, 512_usize, CrossoverInput::Signal, CrossoverImplementation::Df1))]
#[bench::case_simd_10band_512(args = (10_usize, 512_usize, CrossoverInput::Signal, CrossoverImplementation::Simd))]
#[bench::case_scalar_10band_512(args = (10_usize, 512_usize, CrossoverInput::Signal, CrossoverImplementation::Scalar))]
#[bench::case_entry_10band_512(args = (10_usize, 512_usize, CrossoverInput::Signal, CrossoverImplementation::Entry))]
#[bench::case_df1_silence_10band_512(args = (10_usize, 512_usize, CrossoverInput::Silence, CrossoverImplementation::Df1))]
#[bench::case_simd_silence_10band_512(args = (10_usize, 512_usize, CrossoverInput::Silence, CrossoverImplementation::Simd))]
#[bench::case_scalar_silence_10band_512(args = (10_usize, 512_usize, CrossoverInput::Silence, CrossoverImplementation::Scalar))]
#[bench::case_entry_silence_10band_512(args = (10_usize, 512_usize, CrossoverInput::Silence, CrossoverImplementation::Entry))]
#[bench::case_df1_quiet_noise_10band_512(args = (10_usize, 512_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Df1))]
#[bench::case_simd_quiet_noise_10band_512(args = (10_usize, 512_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Simd))]
#[bench::case_scalar_quiet_noise_10band_512(args = (10_usize, 512_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Scalar))]
#[bench::case_entry_quiet_noise_10band_512(args = (10_usize, 512_usize, CrossoverInput::QuietNoise, CrossoverImplementation::Entry))]
fn native_crossover(mut prepared: CrossoverPrepared) -> CrossoverPrepared {
    if let Some(baseline) = &mut prepared.baseline {
        baseline.sum(
            std::hint::black_box(&mut prepared.io),
            std::hint::black_box(&prepared.gains),
            prepared.frames,
        );
    } else {
        prepared
            .svf
            .as_mut()
            .expect("SVF implementation")
            .sum_with(
                std::hint::black_box(&mut prepared.io),
                std::hint::black_box(&prepared.gains),
                prepared.frames,
                prepared.kernel.expect("split kernel"),
            );
    }
    std::hint::black_box(&prepared.io);
    prepared
}

struct DecayPrepared {
    filter: Biquad,
    planes: Vec<Vec<f32>>,
    template: Vec<Vec<f32>>,
}

fn prepare_decay() -> DecayPrepared {
    let mut filter = Biquad::new(consts::TWO, NonZeroUsize::MIN).expect("filter");
    filter.retune(0, low_pass()).expect("section");
    let burst: Vec<f32> = (0..1024)
        .map(|frame| if frame < 64 { 0.5 } else { 0.0 })
        .collect();
    let template = vec![burst; consts::TWO.get()];
    DecayPrepared {
        filter,
        planes: template.clone(),
        template,
    }
}

#[metabench::benchmark(KERNEL_DECAY, "biquad_decay", "case", gungraun_setup = prepare_decay)]
#[bench::case_burst_into_silence_2ch()]
fn native_decay(mut prepared: DecayPrepared) -> DecayPrepared {
    prepared.planes.clone_from(&prepared.template);
    for start in (0..1024).step_by(64) {
        let _ = prepared
            .filter
            .process(&mut prepared.planes, start..start + 64);
    }
    prepared
}

struct SvfPrepared {
    frames: usize,
    response: Response,
    filter: StateVariableFilter<2>,
    planes: [Vec<f32>; 2],
    cutoffs: Vec<f64>,
    rate: NonZeroU32,
}

fn prepare_svf(frames: usize, response: Response) -> SvfPrepared {
    let rate = NonZeroU32::new(48_000).expect("rate");
    let tone: Vec<f32> = std::iter::successors(Some(0.0_f32), |phase| {
        Some(phase + consts::TONE_STEP)
    })
    .take(frames)
    .map(|phase| 0.25 * phase.sin())
    .collect();
    let planes = [tone.clone(), tone];
    let cutoffs = vec![4000.0; frames];
    let mut filter = StateVariableFilter::<2>::new();
    let mut probe = planes.clone();
    assert!(
        filter
            .process(
                &mut probe,
                0..frames,
                &cutoffs,
                rate,
                FRAC_1_SQRT_2,
                response,
            )
            .is_ok()
    );
    assert!(probe.iter().flatten().all(|sample| sample.is_finite()));
    assert!(probe.iter().flatten().any(|sample| *sample != 0.0));
    filter.reset();
    SvfPrepared {
        frames,
        response,
        filter,
        planes,
        cutoffs,
        rate,
    }
}

#[metabench::benchmark(KERNEL_SVF, "svf", "case", gungraun_setup = prepare_svf)]
#[bench::case_2ch_lowpass_128(args = (128_usize, Response::LowPass))]
#[bench::case_2ch_highpass_128(args = (128_usize, Response::HighPass))]
#[bench::case_2ch_lowpass_1024(args = (1024_usize, Response::LowPass))]
#[bench::case_2ch_highpass_1024(args = (1024_usize, Response::HighPass))]
fn native_svf(mut prepared: SvfPrepared) -> SvfPrepared {
    prepared
        .filter
        .process(
            std::hint::black_box(&mut prepared.planes),
            0..prepared.frames,
            &prepared.cutoffs,
            prepared.rate,
            FRAC_1_SQRT_2,
            prepared.response,
        )
        .expect("svf");
    prepared
}

struct InterpPrepared {
    method: Interpolation,
    window: Vec<f32>,
    positions: Vec<f32>,
    output: Vec<f32>,
}

fn prepare_interp(ratio: f64, method: Interpolation) -> InterpPrepared {
    let window: Vec<f32> = std::iter::successors(Some(0.0_f32), |phase| Some(phase + 0.05))
        .map(f32::sin)
        .take(1028)
        .collect();
    let positions: Vec<f32> = std::iter::successors(Some(1.0_f64), |position| {
        Some(position + ratio)
    })
    .take_while(|position| *position < 1024.0)
    .filter_map(|position| position.to_f32())
    .collect();
    InterpPrepared {
        method,
        window,
        output: vec![0.0_f32; positions.len()],
        positions,
    }
}

#[metabench::benchmark(KERNEL_INTERP, "interp", "case", gungraun_setup = prepare_interp)]
#[bench::case_linear_0_5(args = (0.5_f64, Interpolation::Linear))]
#[bench::case_quadratic_0_5(args = (0.5_f64, Interpolation::Quadratic))]
#[bench::case_hermite_0_5(args = (0.5_f64, Interpolation::Hermite))]
#[bench::case_watte_0_5(args = (0.5_f64, Interpolation::Watte))]
#[bench::case_linear_1_001(args = (1.001_f64, Interpolation::Linear))]
#[bench::case_quadratic_1_001(args = (1.001_f64, Interpolation::Quadratic))]
#[bench::case_hermite_1_001(args = (1.001_f64, Interpolation::Hermite))]
#[bench::case_watte_1_001(args = (1.001_f64, Interpolation::Watte))]
#[bench::case_linear_2(args = (2.0_f64, Interpolation::Linear))]
#[bench::case_quadratic_2(args = (2.0_f64, Interpolation::Quadratic))]
#[bench::case_hermite_2(args = (2.0_f64, Interpolation::Hermite))]
#[bench::case_watte_2(args = (2.0_f64, Interpolation::Watte))]
fn native_interpolate(mut prepared: InterpPrepared) -> InterpPrepared {
    interpolate(
        prepared.method,
        &prepared.window,
        &prepared.positions,
        &mut prepared.output,
    )
    .expect("interpolate");
    prepared
}

#[derive(Clone, Copy)]
enum SourceFrameMode {
    Interpolate,
    Weighted,
}

struct SourceFramesPrepared {
    channels: usize,
    mode: SourceFrameMode,
    samples: Vec<f32>,
    offsets: [usize; 33],
    weights: [f64; 33],
    frame: Vec<f32>,
    sums: Vec<f64>,
}

fn prepare_source_frames(channels: usize, mode: SourceFrameMode) -> SourceFramesPrepared {
    SourceFramesPrepared {
        channels,
        mode,
        samples: vec![0.25; channels * 33],
        offsets: std::array::from_fn(|tap| tap * channels),
        weights: [0.125; 33],
        frame: vec![0.0; channels],
        sums: vec![0.0; channels],
    }
}

#[metabench::benchmark(KERNEL_SOURCE_FRAMES, "source_frames", "case", gungraun_setup = prepare_source_frames)]
#[bench::case_interpolate_1(args = (1_usize, SourceFrameMode::Interpolate))]
#[bench::case_weighted_1(args = (1_usize, SourceFrameMode::Weighted))]
#[bench::case_interpolate_2(args = (2_usize, SourceFrameMode::Interpolate))]
#[bench::case_weighted_2(args = (2_usize, SourceFrameMode::Weighted))]
#[bench::case_interpolate_4(args = (4_usize, SourceFrameMode::Interpolate))]
#[bench::case_weighted_4(args = (4_usize, SourceFrameMode::Weighted))]
#[bench::case_interpolate_6(args = (6_usize, SourceFrameMode::Interpolate))]
#[bench::case_weighted_6(args = (6_usize, SourceFrameMode::Weighted))]
#[bench::case_interpolate_9(args = (9_usize, SourceFrameMode::Interpolate))]
#[bench::case_weighted_9(args = (9_usize, SourceFrameMode::Weighted))]
fn native_source_frames(mut prepared: SourceFramesPrepared) -> SourceFramesPrepared {
    let channels = prepared.channels;
    match prepared.mode {
        SourceFrameMode::Interpolate => {
            interpolate_frame(
                &prepared.samples[..channels],
                &prepared.samples[channels..2 * channels],
                0.531_589_7,
                &mut prepared.frame,
            );
            std::hint::black_box(&prepared.frame);
        }
        SourceFrameMode::Weighted => {
            weighted_frames(
                &prepared.samples,
                &prepared.offsets,
                &prepared.weights,
                channels,
                &mut prepared.sums,
            )
            .expect("resolved taps");
            std::hint::black_box(&prepared.sums);
        }
    }
    prepared
}

#[derive(Clone, Copy)]
enum SpectrumImplementation {
    Fft,
    Realfft,
}

enum SpectrumPrepared {
    Fft {
        fft: Fft,
        frame: Vec<f32>,
        bins: kithara_dsp::spectrum::Spectrum,
    },
    Realfft {
        reference: RealToComplexEven<f32>,
        frame: Vec<f32>,
        window: Vec<f32>,
        input: Vec<f32>,
        output: Vec<realfft::num_complex::Complex<f32>>,
        scratch: Vec<realfft::num_complex::Complex<f32>>,
    },
}

fn prepare_spectrum(len: usize, implementation: SpectrumImplementation) -> SpectrumPrepared {
    let region = pools();
    let frame = tone(len);
    match implementation {
        SpectrumImplementation::Fft => {
            let fft = Fft::new(FftLen::new(len).expect("FFT length")).expect("FFT");
            let bins = fft.spectrum(&region).expect("planes");
            SpectrumPrepared::Fft { fft, frame, bins }
        }
        SpectrumImplementation::Realfft => {
            let reference: RealToComplexEven<f32> =
                RealToComplexEven::new(len, &mut rustfft::FftPlanner::new());
            SpectrumPrepared::Realfft {
                input: reference.make_input_vec(),
                output: reference.make_output_vec(),
                scratch: reference.make_scratch_vec(),
                window: hann(len),
                reference,
                frame,
            }
        }
    }
}

#[metabench::benchmark(KERNEL_SPECTRUM, "spectrum", "case", gungraun_setup = prepare_spectrum)]
#[bench::case_fft_1024(args = (1024_usize, SpectrumImplementation::Fft))]
#[bench::case_realfft_1024(args = (1024_usize, SpectrumImplementation::Realfft))]
#[bench::case_fft_3072(args = (3072_usize, SpectrumImplementation::Fft))]
#[bench::case_realfft_3072(args = (3072_usize, SpectrumImplementation::Realfft))]
#[bench::case_fft_4096(args = (4096_usize, SpectrumImplementation::Fft))]
#[bench::case_realfft_4096(args = (4096_usize, SpectrumImplementation::Realfft))]
fn native_spectrum(prepared: SpectrumPrepared) -> SpectrumPrepared {
    match prepared {
        SpectrumPrepared::Fft {
            fft,
            frame,
            mut bins,
        } => {
            fft.forward(std::hint::black_box(&frame), &mut bins)
                .expect("forward");
            SpectrumPrepared::Fft { fft, frame, bins }
        }
        SpectrumPrepared::Realfft {
            reference,
            frame,
            window,
            mut input,
            mut output,
            mut scratch,
        } => {
            for ((slot, sample), weight) in input.iter_mut().zip(&frame).zip(&window) {
                *slot = sample * weight;
            }
            reference
                .process_with_scratch(&mut input, &mut output, &mut scratch)
                .expect("realfft");
            SpectrumPrepared::Realfft {
                reference,
                frame,
                window,
                input,
                output,
                scratch,
            }
        }
    }
}

struct PredictionPrepared {
    re: Vec<f32>,
    im: Vec<f32>,
    history: [Vec<f32>; 4],
}

fn prepare_prediction(size: usize) -> PredictionPrepared {
    let re = tone(size);
    PredictionPrepared {
        im: re.iter().rev().copied().collect(),
        history: std::array::from_fn(|_| vec![0.0; size + 1]),
        re,
    }
}

#[metabench::benchmark(KERNEL_PREDICTION, "prediction_difference", "case", gungraun_setup = prepare_prediction)]
#[bench::case_1(1_usize)]
#[bench::case_3(3_usize)]
#[bench::case_17(17_usize)]
#[bench::case_64(64_usize)]
#[bench::case_513(513_usize)]
#[bench::case_1024(1024_usize)]
fn native_prediction_difference(mut prepared: PredictionPrepared) -> PredictionPrepared {
    prediction_difference(
        [
            std::hint::black_box(&prepared.re),
            std::hint::black_box(&prepared.im),
        ],
        std::hint::black_box(&mut prepared.history)
            .each_mut()
            .map(Vec::as_mut_slice),
    );
    prepared
}

struct AutocorrelationPrepared {
    acf: Autocorrelation,
    frame: Vec<f32>,
    output: Vec<f32>,
}

fn prepare_autocorrelation(size: usize) -> AutocorrelationPrepared {
    let region = pools();
    AutocorrelationPrepared {
        acf: Autocorrelation::new(NonZeroUsize::new(size).expect("size"), &region)
            .expect("acf"),
        frame: tone(size),
        output: vec![0.0_f32; size],
    }
}

#[metabench::benchmark(KERNEL_AUTOCORRELATION, "autocorrelation", "case", gungraun_setup = prepare_autocorrelation)]
#[bench::case_process_64(64_usize)]
#[bench::case_process_128(128_usize)]
#[bench::case_process_256(256_usize)]
#[bench::case_process_512(512_usize)]
#[bench::case_process_1024(1024_usize)]
#[bench::case_process_4096(4096_usize)]
fn native_autocorrelation(mut prepared: AutocorrelationPrepared) -> AutocorrelationPrepared {
    prepared
        .acf
        .process(std::hint::black_box(&prepared.frame), &mut prepared.output);
    prepared
}
