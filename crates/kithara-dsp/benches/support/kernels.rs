struct NativeWork {
    action: Box<dyn FnMut()>,
}

impl NativeWork {
    fn new(action: impl FnMut() + 'static) -> Self {
        Self { action: Box::new(action) }
    }

    fn run(mut self) -> Self {
        (self.action)();
        self
    }
}

fn prepare_last_loud(index: usize) -> NativeWork {
    let size = [128_usize, 512, 4096][index / 4];
    let shape = index % 4 / 2;
    let implementation = index % 2;
    let mut samples = vec![0.0_f32; size];
    if shape == 1 {
        *samples.last_mut().expect("nonempty") = 0.25;
    }
    let expected = loud_scalar(&samples, 1e-6);
    let search: fn(&[f32], f32) -> Option<usize> = if implementation == 0 {
        kithara_dsp::last_loud
    } else {
        loud_scalar
    };
    assert_eq!(search(&samples, 1e-6), expected);
    NativeWork::new(move || {
        std::hint::black_box(search(
            std::hint::black_box(&samples),
            std::hint::black_box(1e-6),
        ));
    })
}

#[metabench::benchmark(KERNEL_LAST_LOUD, "last_loud", "case", gungraun_setup = prepare_last_loud)]
#[bench::case_silent_native_128(0_usize)]
#[bench::case_silent_rposition_128(1_usize)]
#[bench::case_last_native_128(2_usize)]
#[bench::case_last_rposition_128(3_usize)]
#[bench::case_silent_native_512(4_usize)]
#[bench::case_silent_rposition_512(5_usize)]
#[bench::case_last_native_512(6_usize)]
#[bench::case_last_rposition_512(7_usize)]
#[bench::case_silent_native_4096(8_usize)]
#[bench::case_silent_rposition_4096(9_usize)]
#[bench::case_last_native_4096(10_usize)]
#[bench::case_last_rposition_4096(11_usize)]
fn native_work_0(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_svf_fixed(index: usize) -> NativeWork {
    let frames = [128_usize, 512][index / 6];
    let response = [Response::LowPass, Response::HighPass][index % 6 / 3];
    let mode = index % 3;
    let rate = NonZeroU32::new(48_000).expect("rate");
    let samples: Vec<_> = tone(frames).into_iter().map(|sample| 0.25 * sample).collect();
    let template = [samples.clone(), samples];
    let cutoffs = vec![4000.0; frames];
    let design = SvfDesign::new(rate, 4000.0, FRAC_1_SQRT_2, response).expect("design");
    let gain = (std::f64::consts::PI * 4000.0 / f64::from(rate.get())).tan();
    let damping = FRAC_1_SQRT_2.recip();
    let a1 = (1.0 + gain * (gain + damping)).recip();
    let a2 = gain * a1;
    let coefficients = [a1, a2, gain * a2, damping];
    let mut filter = StateVariableFilter::<2>::new();
    let mut planes = template;
    let mut state = [[0.0_f64; 2]; 2];
    NativeWork::new(move || {
        match mode {
            0 => filter.process_fixed(std::hint::black_box(&mut planes), 0..frames, &design).expect("fixed"),
            1 => filter.process(std::hint::black_box(&mut planes), 0..frames, &cutoffs, rate, FRAC_1_SQRT_2, response).expect("dynamic"),
            _ => scalar_svf_fixed(&mut planes, &mut state, &coefficients, response),
        };
        std::hint::black_box(&planes);
    })
}

#[metabench::benchmark(KERNEL_SVF_FIXED, "svf_fixed", "case", gungraun_setup = prepare_svf_fixed)]
#[bench::case_2ch_lowpass_fixed_128(0_usize)]
#[bench::case_2ch_lowpass_per_frame_128(1_usize)]
#[bench::case_2ch_lowpass_scalar_128(2_usize)]
#[bench::case_2ch_highpass_fixed_128(3_usize)]
#[bench::case_2ch_highpass_per_frame_128(4_usize)]
#[bench::case_2ch_highpass_scalar_128(5_usize)]
#[bench::case_2ch_lowpass_fixed_512(6_usize)]
#[bench::case_2ch_lowpass_per_frame_512(7_usize)]
#[bench::case_2ch_lowpass_scalar_512(8_usize)]
#[bench::case_2ch_highpass_fixed_512(9_usize)]
#[bench::case_2ch_highpass_per_frame_512(10_usize)]
#[bench::case_2ch_highpass_scalar_512(11_usize)]
fn native_work_1(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_crossfade(index: usize, kernel: Crossfade) -> NativeWork {
    let old_sample = 0.314_159_f32;
    let new_sample = -0.271_828_f32;
    let mut shapes = Vec::new();
    for frames in consts::SIZES {
        for channels in [NonZeroUsize::MIN, consts::TWO, consts::SIX] {
            shapes.push((channels, frames, 0, frames, new_sample));
        }
    }
    shapes.extend([
        (consts::TWO, 7, 0, 7, new_sample),
        (consts::SIX, 127, 0, 127, new_sample),
        (consts::TWO, 127, 1, 127, new_sample),
        (consts::TWO, 4096, 0, 128, new_sample),
        (consts::TWO, 7, 0, 7, old_sample),
    ]);
    let (channels, frames, tail, chunk, input_sample) = shapes[index];
    let old = vec![old_sample; frames * channels.get() + tail];
    let apply = {
        let old = old.clone();
        move |output: &mut [f32]| {
        let mut processed = 0;
        for (outgoing, incoming) in old
            .chunks(chunk * channels.get())
            .zip(output.chunks_mut(chunk * channels.get()))
        {
            processed += kernel(outgoing, incoming, channels, processed, frames);
        }
            processed
        }
    };
    let mut expected = vec![input_sample; frames * channels.get() + tail];
    assert_eq!(previous_audio_crossfade(&old, &mut expected, channels, 0, frames), frames);
    let mut actual = vec![input_sample; expected.len()];
    assert_eq!(apply(&mut actual), frames);
    assert!(actual.iter().zip(&expected).all(|(a,b)| a.to_bits() == b.to_bits()));
    let mut output = vec![input_sample; expected.len()];
    NativeWork::new(move || {
        std::hint::black_box(apply(std::hint::black_box(&mut output)));
    })
}

#[metabench::benchmark(KERNEL_CROSSFADE_LINEAR, "crossfade_linear", "case", gungraun_setup = prepare_linear)]
#[bench::case_1ch_64(0_usize)]
#[bench::case_2ch_64(1_usize)]
#[bench::case_6ch_64(2_usize)]
#[bench::case_1ch_128(3_usize)]
#[bench::case_2ch_128(4_usize)]
#[bench::case_6ch_128(5_usize)]
#[bench::case_1ch_256(6_usize)]
#[bench::case_2ch_256(7_usize)]
#[bench::case_6ch_256(8_usize)]
#[bench::case_1ch_512(9_usize)]
#[bench::case_2ch_512(10_usize)]
#[bench::case_6ch_512(11_usize)]
#[bench::case_1ch_1024(12_usize)]
#[bench::case_2ch_1024(13_usize)]
#[bench::case_6ch_1024(14_usize)]
#[bench::case_1ch_4096(15_usize)]
#[bench::case_2ch_4096(16_usize)]
#[bench::case_6ch_4096(17_usize)]
#[bench::case_2ch_7(18_usize)]
#[bench::case_6ch_127(19_usize)]
#[bench::case_2ch_127_tail(20_usize)]
#[bench::case_2ch_4096_chunk128(21_usize)]
#[bench::case_2ch_7_identity(22_usize)]
fn native_work_2(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_linear(index: usize) -> NativeWork {
    prepare_crossfade(index, kithara_dsp::crossfade_linear)
}

#[metabench::benchmark(KERNEL_CROSSFADE_PREVIOUS, "crossfade_previous_audio", "case", gungraun_setup = prepare_previous)]
#[bench::case_1ch_64(0_usize)]
#[bench::case_2ch_64(1_usize)]
#[bench::case_6ch_64(2_usize)]
#[bench::case_1ch_128(3_usize)]
#[bench::case_2ch_128(4_usize)]
#[bench::case_6ch_128(5_usize)]
#[bench::case_1ch_256(6_usize)]
#[bench::case_2ch_256(7_usize)]
#[bench::case_6ch_256(8_usize)]
#[bench::case_1ch_512(9_usize)]
#[bench::case_2ch_512(10_usize)]
#[bench::case_6ch_512(11_usize)]
#[bench::case_1ch_1024(12_usize)]
#[bench::case_2ch_1024(13_usize)]
#[bench::case_6ch_1024(14_usize)]
#[bench::case_1ch_4096(15_usize)]
#[bench::case_2ch_4096(16_usize)]
#[bench::case_6ch_4096(17_usize)]
#[bench::case_2ch_7(18_usize)]
#[bench::case_6ch_127(19_usize)]
#[bench::case_2ch_127_tail(20_usize)]
#[bench::case_2ch_4096_chunk128(21_usize)]
#[bench::case_2ch_7_identity(22_usize)]
fn native_work_3(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_previous(index: usize) -> NativeWork {
    prepare_crossfade(index, previous_audio_crossfade)
}

fn prepare_layout(index: usize) -> NativeWork {
    if index >= 48 {
        let frames = consts::SIZES[index - 48];
        let mut noisy = vec![f32::from_bits(1); frames];
        return NativeWork::new(move || kithara_dsp::sanitize(std::hint::black_box(&mut noisy)));
    }
    let frames = consts::SIZES[index / 8];
    let channels = [consts::TWO, consts::SIX][index % 8 / 4];
    let operation = index % 4;
    let planes = vec![vec![0.25_f32; frames]; consts::SIX.get()];
    let planar = planes.concat();
    let stride = NonZeroUsize::new(frames).expect("stride");
    let mut restored = planes.clone();
    let mut interleaved = vec![0.0_f32; channels.get() * frames];
    NativeWork::new(move || {
        match operation {
            0 => fast_interleave::interleave_variable(
                std::hint::black_box(&planes[..channels.get()]), 0..frames, &mut interleaved, channels),
            1 => fast_interleave::deinterleave_variable(
                std::hint::black_box(&interleaved), channels, &mut restored, 0..frames),
            2 => kithara_dsp::interleave_channel_major(
                std::hint::black_box(&planar[..channels.get() * frames]), stride, 0..frames, &mut interleaved, channels),
            _ => kithara_dsp::deinterleave_variable(
                std::hint::black_box(&interleaved), channels, &mut restored, 0..frames),
        }
    })
}

#[metabench::benchmark(KERNEL_LAYOUT, "layout", "case", gungraun_setup = prepare_layout)]
#[bench::case_fi_interleave_2ch_64(0_usize)]
#[bench::case_fi_deinterleave_2ch_64(1_usize)]
#[bench::case_interleave_2ch_64(2_usize)]
#[bench::case_deinterleave_2ch_64(3_usize)]
#[bench::case_fi_interleave_6ch_64(4_usize)]
#[bench::case_fi_deinterleave_6ch_64(5_usize)]
#[bench::case_interleave_6ch_64(6_usize)]
#[bench::case_deinterleave_6ch_64(7_usize)]
#[bench::case_sanitize_64(8_usize)]
#[bench::case_fi_interleave_2ch_128(9_usize)]
#[bench::case_fi_deinterleave_2ch_128(10_usize)]
#[bench::case_interleave_2ch_128(11_usize)]
#[bench::case_deinterleave_2ch_128(12_usize)]
#[bench::case_fi_interleave_6ch_128(13_usize)]
#[bench::case_fi_deinterleave_6ch_128(14_usize)]
#[bench::case_interleave_6ch_128(15_usize)]
#[bench::case_deinterleave_6ch_128(16_usize)]
#[bench::case_sanitize_128(17_usize)]
#[bench::case_fi_interleave_2ch_256(18_usize)]
#[bench::case_fi_deinterleave_2ch_256(19_usize)]
#[bench::case_interleave_2ch_256(20_usize)]
#[bench::case_deinterleave_2ch_256(21_usize)]
#[bench::case_fi_interleave_6ch_256(22_usize)]
#[bench::case_fi_deinterleave_6ch_256(23_usize)]
#[bench::case_interleave_6ch_256(24_usize)]
#[bench::case_deinterleave_6ch_256(25_usize)]
#[bench::case_sanitize_256(26_usize)]
#[bench::case_fi_interleave_2ch_512(27_usize)]
#[bench::case_fi_deinterleave_2ch_512(28_usize)]
#[bench::case_interleave_2ch_512(29_usize)]
#[bench::case_deinterleave_2ch_512(30_usize)]
#[bench::case_fi_interleave_6ch_512(31_usize)]
#[bench::case_fi_deinterleave_6ch_512(32_usize)]
#[bench::case_interleave_6ch_512(33_usize)]
#[bench::case_deinterleave_6ch_512(34_usize)]
#[bench::case_sanitize_512(35_usize)]
#[bench::case_fi_interleave_2ch_1024(36_usize)]
#[bench::case_fi_deinterleave_2ch_1024(37_usize)]
#[bench::case_interleave_2ch_1024(38_usize)]
#[bench::case_deinterleave_2ch_1024(39_usize)]
#[bench::case_fi_interleave_6ch_1024(40_usize)]
#[bench::case_fi_deinterleave_6ch_1024(41_usize)]
#[bench::case_interleave_6ch_1024(42_usize)]
#[bench::case_deinterleave_6ch_1024(43_usize)]
#[bench::case_sanitize_1024(44_usize)]
#[bench::case_fi_interleave_2ch_4096(45_usize)]
#[bench::case_fi_deinterleave_2ch_4096(46_usize)]
#[bench::case_interleave_2ch_4096(47_usize)]
#[bench::case_deinterleave_2ch_4096(48_usize)]
#[bench::case_fi_interleave_6ch_4096(49_usize)]
#[bench::case_fi_deinterleave_6ch_4096(50_usize)]
#[bench::case_interleave_6ch_4096(51_usize)]
#[bench::case_deinterleave_6ch_4096(52_usize)]
#[bench::case_sanitize_4096(53_usize)]
fn native_work_4(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_vector(size: usize) -> NativeWork {
    let samples = tone(size);
    NativeWork::new(move || {
        let _ = kithara_dsp::sum_squares(std::hint::black_box(&samples));
    })
}

#[metabench::benchmark(KERNEL_VECTOR, "vector", "case", gungraun_setup = prepare_vector)]
#[bench::case_sum_squares_64(0_usize)]
#[bench::case_sum_squares_128(1_usize)]
#[bench::case_sum_squares_256(2_usize)]
#[bench::case_sum_squares_512(3_usize)]
#[bench::case_sum_squares_1024(4_usize)]
#[bench::case_sum_squares_4096(5_usize)]
fn native_work_5(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_curve_hold(index: usize) -> NativeWork {
    let frames = [128_usize, 512][index % 2];
    let implementation = index / 2;
    let positions: Vec<_> = (0..frames as u32)
        .map(|frame| f64::from(frame) / f64::from(frames as u32 - 1))
        .collect();
    let shape = CurveShape::HOLD;
    let mut values = positions;
    NativeWork::new(move || {
        match implementation {
            0 => shape.process(std::hint::black_box(&mut values), std::hint::black_box([0.75, 0.25])),
            1 => scalar_curve("hold", std::hint::black_box(&mut values), std::hint::black_box([0.75, 0.25])),
            _ => {
                let kernel = kithara_dsp::routes::parameter_hold::routes().nth(implementation - 2).expect("curve route").1;
                kernel(std::hint::black_box(&mut values), std::hint::black_box([0.75, 0.25]));
            }
        }
    })
}

#[metabench::benchmark(KERNEL_CURVE_HOLD, "curve_hold", "case", gungraun_setup = prepare_curve_hold)]
#[bench::case_native_128(0_usize)]
#[bench::case_scalar_128(1_usize)]
#[bench::case_simd_128(2_usize)]
#[bench::case_entry_128(3_usize)]
#[bench::case_native_512(4_usize)]
#[bench::case_scalar_512(5_usize)]
#[bench::case_simd_512(6_usize)]
#[bench::case_entry_512(7_usize)]
fn native_work_6(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_curve_linear(index: usize) -> NativeWork {
    let frames = [128_usize, 512][index % 2];
    let implementation = index / 2;
    let positions: Vec<_> = (0..frames as u32)
        .map(|frame| f64::from(frame) / f64::from(frames as u32 - 1))
        .collect();
    let shape = CurveShape::LINEAR;
    let mut values = positions;
    NativeWork::new(move || {
        match implementation {
            0 => shape.process(std::hint::black_box(&mut values), std::hint::black_box([0.75, 0.25])),
            1 => scalar_curve("linear", std::hint::black_box(&mut values), std::hint::black_box([0.75, 0.25])),
            _ => {
                let kernel = kithara_dsp::routes::parameter_linear::routes().nth(implementation - 2).expect("curve route").1;
                kernel(std::hint::black_box(&mut values), std::hint::black_box([0.75, 0.25]));
            }
        }
    })
}

#[metabench::benchmark(KERNEL_CURVE_LINEAR, "curve_linear", "case", gungraun_setup = prepare_curve_linear)]
#[bench::case_native_128(0_usize)]
#[bench::case_scalar_128(1_usize)]
#[bench::case_simd_128(2_usize)]
#[bench::case_entry_128(3_usize)]
#[bench::case_native_512(4_usize)]
#[bench::case_scalar_512(5_usize)]
#[bench::case_simd_512(6_usize)]
#[bench::case_entry_512(7_usize)]
fn native_work_7(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_curve_cubic(index: usize) -> NativeWork {
    let frames = [128_usize, 512][index % 2];
    let implementation = index / 2;
    let positions: Vec<_> = (0..frames as u32)
        .map(|frame| f64::from(frame) / f64::from(frames as u32 - 1))
        .collect();
    let shape = CurveShape::cubic(0.2, 0.6).expect("ordered handles");
    let mut values = positions;
    NativeWork::new(move || {
        match implementation {
            0 => shape.process(std::hint::black_box(&mut values), std::hint::black_box([0.75, 0.25])),
            1 => scalar_curve("cubic", std::hint::black_box(&mut values), std::hint::black_box([0.75, 0.25])),
            _ => unreachable!("cubic has no routed form"),
        }
    })
}

#[metabench::benchmark(KERNEL_CURVE_CUBIC, "curve_cubic", "case", gungraun_setup = prepare_curve_cubic)]
#[bench::case_native_128(0_usize)]
#[bench::case_scalar_128(1_usize)]
#[bench::case_native_512(2_usize)]
#[bench::case_scalar_512(3_usize)]
fn native_work_8(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_ratio_ramp(index: usize) -> NativeWork {
    let implementation = index / 2;
    let native: kithara_dsp::routes::ratio_ramp::Kernel = kithara_dsp::ratio_ramp;
    let kernel = kithara_dsp::routes::ratio_ramp::routes()
        .chain([("native", native)])
        .nth(implementation)
        .expect("ratio route")
        .1;
    let mut output = vec![0.0; [128_usize, 512][index % 2]];
    NativeWork::new(move || {
        kernel(
            std::hint::black_box(&mut output),
            std::hint::black_box((1_u64 << 25) + 3),
            std::hint::black_box(NonZeroU64::new(997).expect("positive")),
        );
    })
}

#[metabench::benchmark(KERNEL_RATIO_RAMP, "ratio_ramp", "case", gungraun_setup = prepare_ratio_ramp)]
#[bench::case_simd_128(0_usize)]
#[bench::case_scalar_128(1_usize)]
#[bench::case_entry_128(2_usize)]
#[bench::case_native_128(3_usize)]
#[bench::case_simd_512(4_usize)]
#[bench::case_scalar_512(5_usize)]
#[bench::case_entry_512(6_usize)]
#[bench::case_native_512(7_usize)]
fn native_work_9(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_narrow(index: usize) -> NativeWork {
    let frames = [128_usize, 512][index % 2];
    let native = index < 2;
    let input: Vec<_> = tone(frames).into_iter().map(f64::from).collect();
    let mut output = vec![0.0; frames];
    NativeWork::new(move || {
        if native {
            kithara_dsp::narrow_f64(&input, &mut output);
        } else {
            for (value, input) in output.iter_mut().zip(&input) {
                *value = input.as_();
            }
        }
    })
}

#[metabench::benchmark(KERNEL_NARROW, "narrow_f64", "case", gungraun_setup = prepare_narrow)]
#[bench::case_native_128(0_usize)]
#[bench::case_scalar_128(1_usize)]
#[bench::case_native_512(2_usize)]
#[bench::case_scalar_512(3_usize)]
fn native_work_10(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_multiply_squared(index: usize) -> NativeWork {
    let frames = [128_usize, 512][index % 2];
    let native = index < 2;
    let mut output = tone(frames);
    let square = vec![0.7; frames];
    let gains = vec![0.3; frames];
    NativeWork::new(move || {
        if native {
            kithara_dsp::multiply_squared_in_place(&mut output, &square, &gains);
        } else {
            for ((value, square), gain) in output.iter_mut().zip(&square).zip(&gains) {
                *value = (*value * (square * square)) * gain;
            }
        }
    })
}

#[metabench::benchmark(KERNEL_MULTIPLY_SQUARED, "multiply_squared_in_place", "case", gungraun_setup = prepare_multiply_squared)]
#[bench::case_native_128(0_usize)]
#[bench::case_scalar_128(1_usize)]
#[bench::case_native_512(2_usize)]
#[bench::case_scalar_512(3_usize)]
fn native_work_11(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_ramp(index: usize) -> NativeWork {
    let frames = [128_usize, 512][index % 2];
    let kernel = kithara_dsp::routes::ramp::routes().nth(index / 2).expect("route").1;
    let input = tone(frames);
    let mut output = if "ramp" == "mul_add" { vec![0.25; frames] } else { input.clone() };
    NativeWork::new(move || {
        kernel(std::hint::black_box(&mut output), std::hint::black_box(0.25), std::hint::black_box(0.001));
        std::hint::black_box(&input);
    })
}

#[metabench::benchmark(KERNEL_RAMP, "ramp", "case", gungraun_setup = prepare_ramp)]
#[bench::case_simd_128(0_usize)]
#[bench::case_entry_128(1_usize)]
#[bench::case_simd_512(2_usize)]
#[bench::case_entry_512(3_usize)]
fn native_work_12(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_div(index: usize) -> NativeWork {
    let frames = [128_usize, 512][index % 2];
    let kernel = kithara_dsp::routes::div::routes().nth(index / 2).expect("route").1;
    let input = tone(frames);
    let mut output = if "div" == "mul_add" { vec![0.25; frames] } else { input.clone() };
    NativeWork::new(move || {
        kernel(std::hint::black_box(&mut output), std::hint::black_box(1.25));
        std::hint::black_box(&input);
    })
}

#[metabench::benchmark(KERNEL_DIV, "div", "case", gungraun_setup = prepare_div)]
#[bench::case_simd_128(0_usize)]
#[bench::case_entry_128(1_usize)]
#[bench::case_simd_512(2_usize)]
#[bench::case_entry_512(3_usize)]
fn native_work_13(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_mul_add(index: usize) -> NativeWork {
    let frames = [128_usize, 512][index % 2];
    let kernel = kithara_dsp::routes::mul_add::routes().nth(index / 2).expect("route").1;
    let input = tone(frames);
    let mut output = if "mul_add" == "mul_add" { vec![0.25; frames] } else { input.clone() };
    NativeWork::new(move || {
        kernel(std::hint::black_box(&input), std::hint::black_box(&input), std::hint::black_box(&mut output));
        std::hint::black_box(&input);
    })
}

#[metabench::benchmark(KERNEL_MUL_ADD, "mul_add", "case", gungraun_setup = prepare_mul_add)]
#[bench::case_simd_128(0_usize)]
#[bench::case_entry_128(1_usize)]
#[bench::case_simd_512(2_usize)]
#[bench::case_entry_512(3_usize)]
fn native_work_14(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_downmix(index: usize) -> NativeWork {
    let frames = consts::SIZES[index / 2];
    let channels = [consts::TWO, consts::SIX][index % 2];
    let interleaved = tone(channels.get() * frames);
    let mut mono = vec![0.0_f32; frames];
    NativeWork::new(move || {
        let _ = kithara_dsp::downmix(std::hint::black_box(&interleaved), channels, &mut mono);
    })
}

#[metabench::benchmark(KERNEL_DOWNMIX, "downmix", "case", gungraun_setup = prepare_downmix)]
#[bench::case_2ch_64(0_usize)]
#[bench::case_6ch_64(1_usize)]
#[bench::case_2ch_128(2_usize)]
#[bench::case_6ch_128(3_usize)]
#[bench::case_2ch_256(4_usize)]
#[bench::case_6ch_256(5_usize)]
#[bench::case_2ch_512(6_usize)]
#[bench::case_6ch_512(7_usize)]
#[bench::case_2ch_1024(8_usize)]
#[bench::case_6ch_1024(9_usize)]
#[bench::case_2ch_4096(10_usize)]
#[bench::case_6ch_4096(11_usize)]
fn native_work_15(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_biquad(index: usize) -> NativeWork {
    let channels = [1_usize, 2, 8][index / 2];
    let sections = [1_usize, 4][index % 2];
    let mut filter = Biquad::new(
        NonZeroUsize::new(channels).expect("channels"),
        NonZeroUsize::new(sections).expect("sections"),
    ).expect("filter");
    for section in 0..sections {
        filter.retune(section, low_pass()).expect("section");
    }
    let mut planes = vec![vec![0.25_f32; 1024]; channels];
    NativeWork::new(move || {
        filter.process(std::hint::black_box(&mut planes), 0..1024).expect("process");
    })
}

#[metabench::benchmark(KERNEL_BIQUAD, "biquad", "case", gungraun_setup = prepare_biquad)]
#[bench::case_1ch_1(0_usize)]
#[bench::case_1ch_4(1_usize)]
#[bench::case_2ch_1(2_usize)]
#[bench::case_2ch_4(3_usize)]
#[bench::case_8ch_1(4_usize)]
#[bench::case_8ch_4(5_usize)]
fn native_work_16(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_crossover(index: usize) -> NativeWork {
    let bands = [3_usize, 10][index / 24];
    let frames = [128_usize, 512][index % 24 / 12];
    let form = index % 12 / 4;
    let implementation = index % 4;
    let frequencies = frequencies(bands);
    let inputs = input_forms(frames);
    let input = inputs[form].1.clone();
    let gains = gains(bands, frames, true);
    if implementation == 0 {
        let mut baseline = Frame::new(&frequencies);
        let mut io = input;
        return NativeWork::new(move || {
            baseline.sum(std::hint::black_box(&mut io), std::hint::black_box(&gains), frames);
            std::hint::black_box(&io);
        });
    }
    let kernel = kithara_dsp::routes::crossover::forms::<2, Vec<f32>>()[implementation - 1].1;
    let mut svf = Svf::new(&frequencies, frames);
    let mut io = input;
    NativeWork::new(move || {
        svf.sum_with(std::hint::black_box(&mut io), std::hint::black_box(&gains), frames, kernel);
        std::hint::black_box(&io);
    })
}

#[metabench::benchmark(KERNEL_CROSSOVER, "crossover_df1_svf", "case", gungraun_setup = prepare_crossover)]
#[bench::case_df1_3band_128(0_usize)]
#[bench::case_simd_3band_128(1_usize)]
#[bench::case_scalar_3band_128(2_usize)]
#[bench::case_entry_3band_128(3_usize)]
#[bench::case_df1_silence_3band_128(4_usize)]
#[bench::case_simd_silence_3band_128(5_usize)]
#[bench::case_scalar_silence_3band_128(6_usize)]
#[bench::case_entry_silence_3band_128(7_usize)]
#[bench::case_df1_quiet_noise_3band_128(8_usize)]
#[bench::case_simd_quiet_noise_3band_128(9_usize)]
#[bench::case_scalar_quiet_noise_3band_128(10_usize)]
#[bench::case_entry_quiet_noise_3band_128(11_usize)]
#[bench::case_df1_3band_512(12_usize)]
#[bench::case_simd_3band_512(13_usize)]
#[bench::case_scalar_3band_512(14_usize)]
#[bench::case_entry_3band_512(15_usize)]
#[bench::case_df1_silence_3band_512(16_usize)]
#[bench::case_simd_silence_3band_512(17_usize)]
#[bench::case_scalar_silence_3band_512(18_usize)]
#[bench::case_entry_silence_3band_512(19_usize)]
#[bench::case_df1_quiet_noise_3band_512(20_usize)]
#[bench::case_simd_quiet_noise_3band_512(21_usize)]
#[bench::case_scalar_quiet_noise_3band_512(22_usize)]
#[bench::case_entry_quiet_noise_3band_512(23_usize)]
#[bench::case_df1_10band_128(24_usize)]
#[bench::case_simd_10band_128(25_usize)]
#[bench::case_scalar_10band_128(26_usize)]
#[bench::case_entry_10band_128(27_usize)]
#[bench::case_df1_silence_10band_128(28_usize)]
#[bench::case_simd_silence_10band_128(29_usize)]
#[bench::case_scalar_silence_10band_128(30_usize)]
#[bench::case_entry_silence_10band_128(31_usize)]
#[bench::case_df1_quiet_noise_10band_128(32_usize)]
#[bench::case_simd_quiet_noise_10band_128(33_usize)]
#[bench::case_scalar_quiet_noise_10band_128(34_usize)]
#[bench::case_entry_quiet_noise_10band_128(35_usize)]
#[bench::case_df1_10band_512(36_usize)]
#[bench::case_simd_10band_512(37_usize)]
#[bench::case_scalar_10band_512(38_usize)]
#[bench::case_entry_10band_512(39_usize)]
#[bench::case_df1_silence_10band_512(40_usize)]
#[bench::case_simd_silence_10band_512(41_usize)]
#[bench::case_scalar_silence_10band_512(42_usize)]
#[bench::case_entry_silence_10band_512(43_usize)]
#[bench::case_df1_quiet_noise_10band_512(44_usize)]
#[bench::case_simd_quiet_noise_10band_512(45_usize)]
#[bench::case_scalar_quiet_noise_10band_512(46_usize)]
#[bench::case_entry_quiet_noise_10band_512(47_usize)]
fn native_work_17(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_decay(_: usize) -> NativeWork {
    let mut filter = Biquad::new(consts::TWO, NonZeroUsize::MIN).expect("filter");
    filter.retune(0, low_pass()).expect("section");
    let burst: Vec<f32> = (0..1024).map(|frame| if frame < 64 { 0.5 } else { 0.0 }).collect();
    let template = vec![burst; consts::TWO.get()];
    let mut planes = template.clone();
    NativeWork::new(move || {
        planes.clone_from(&template);
        for start in (0..1024).step_by(64) {
            let _ = filter.process(&mut planes, start..start + 64);
        }
    })
}

#[metabench::benchmark(KERNEL_DECAY, "biquad_decay", "case", gungraun_setup = prepare_decay)]
#[bench::case_burst_into_silence_2ch(0_usize)]
fn native_work_18(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_svf(index: usize) -> NativeWork {
    let frames = [128_usize, 1024][index / 2];
    let response = [Response::LowPass, Response::HighPass][index % 2];
    let rate = NonZeroU32::new(48_000).expect("rate");
    let tone: Vec<f32> = std::iter::successors(Some(0.0_f32), |phase| Some(phase + consts::TONE_STEP))
        .take(frames).map(|phase| 0.25 * phase.sin()).collect();
    let mut planes = [tone.clone(), tone];
    let cutoffs = vec![4000.0; frames];
    let mut filter = StateVariableFilter::<2>::new();
    let mut probe = planes.clone();
    assert!(filter.process(&mut probe, 0..frames, &cutoffs, rate, FRAC_1_SQRT_2, response).is_ok());
    assert!(probe.iter().flatten().all(|sample| sample.is_finite()));
    assert!(probe.iter().flatten().any(|sample| *sample != 0.0));
    filter.reset();
    NativeWork::new(move || {
        filter.process(&mut planes, 0..frames, &cutoffs, rate, FRAC_1_SQRT_2, response).expect("svf");
    })
}

#[metabench::benchmark(KERNEL_SVF, "svf", "case", gungraun_setup = prepare_svf)]
#[bench::case_2ch_lowpass_128(0_usize)]
#[bench::case_2ch_highpass_128(1_usize)]
#[bench::case_2ch_lowpass_1024(2_usize)]
#[bench::case_2ch_highpass_1024(3_usize)]
fn native_work_19(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_interp(index: usize) -> NativeWork {
    let ratio = [0.5_f64, 1.001, 2.0][index / 4];
    let method = [Interpolation::Linear, Interpolation::Quadratic, Interpolation::Hermite, Interpolation::Watte][index % 4];
    let window: Vec<f32> = std::iter::successors(Some(0.0_f32), |phase| Some(phase + 0.05))
        .map(f32::sin).take(1028).collect();
    let positions: Vec<f32> = std::iter::successors(Some(1.0_f64), |position| Some(position + ratio))
        .take_while(|position| *position < 1024.0).filter_map(|position| position.to_f32()).collect();
    let mut output = vec![0.0_f32; positions.len()];
    NativeWork::new(move || {
        interpolate(method, &window, &positions, &mut output).expect("interpolate");
    })
}

#[metabench::benchmark(KERNEL_INTERP, "interp", "case", gungraun_setup = prepare_interp)]
#[bench::case_linear_0_5(0_usize)]
#[bench::case_quadratic_0_5(1_usize)]
#[bench::case_hermite_0_5(2_usize)]
#[bench::case_watte_0_5(3_usize)]
#[bench::case_linear_1_001(4_usize)]
#[bench::case_quadratic_1_001(5_usize)]
#[bench::case_hermite_1_001(6_usize)]
#[bench::case_watte_1_001(7_usize)]
#[bench::case_linear_2(8_usize)]
#[bench::case_quadratic_2(9_usize)]
#[bench::case_hermite_2(10_usize)]
#[bench::case_watte_2(11_usize)]
fn native_work_20(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_source_frames(index: usize) -> NativeWork {
    let channels = [1_usize, 2, 4, 6, 9][index / 2];
    let weighted = index % 2 == 1;
    let samples = vec![0.25; channels * 33];
    let offsets = std::array::from_fn::<_, 33, _>(|tap| tap * channels);
    let weights = [0.125; 33];
    let mut frame = vec![0.0; channels];
    let mut sums = vec![0.0; channels];
    NativeWork::new(move || {
        if weighted {
            weighted_frames(&samples, &offsets, &weights, channels, &mut sums).expect("resolved taps");
            std::hint::black_box(&sums);
        } else {
            interpolate_frame(&samples[..channels], &samples[channels..2 * channels], 0.531_589_7, &mut frame);
            std::hint::black_box(&frame);
        }
    })
}

#[metabench::benchmark(KERNEL_SOURCE_FRAMES, "source_frames", "case", gungraun_setup = prepare_source_frames)]
#[bench::case_interpolate_1(0_usize)]
#[bench::case_weighted_1(1_usize)]
#[bench::case_interpolate_2(2_usize)]
#[bench::case_weighted_2(3_usize)]
#[bench::case_interpolate_4(4_usize)]
#[bench::case_weighted_4(5_usize)]
#[bench::case_interpolate_6(6_usize)]
#[bench::case_weighted_6(7_usize)]
#[bench::case_interpolate_9(8_usize)]
#[bench::case_weighted_9(9_usize)]
fn native_work_21(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_spectrum(index: usize) -> NativeWork {
    let len = consts::FFT_LENS[index / 2];
    let native = index % 2 == 0;
    let region = pools();
    let frame = tone(len);
    let window = hann(len);
    if native {
        let fft = Fft::new(FftLen::new(len).expect("FFT length")).expect("FFT");
        let mut bins = fft.spectrum(&region).expect("planes");
        return NativeWork::new(move || {
            fft.forward(std::hint::black_box(&frame), &mut bins).expect("forward");
        });
    }
    let reference: RealToComplexEven<f32> = RealToComplexEven::new(len, &mut rustfft::FftPlanner::new());
    let mut input = reference.make_input_vec();
    let mut output = reference.make_output_vec();
    let mut scratch = reference.make_scratch_vec();
    NativeWork::new(move || {
        for ((slot, sample), weight) in input.iter_mut().zip(&frame).zip(&window) {
            *slot = sample * weight;
        }
        reference.process_with_scratch(&mut input, &mut output, &mut scratch).expect("realfft");
    })
}

#[metabench::benchmark(KERNEL_SPECTRUM, "spectrum", "case", gungraun_setup = prepare_spectrum)]
#[bench::case_fft_1024(0_usize)]
#[bench::case_realfft_1024(1_usize)]
#[bench::case_fft_3072(2_usize)]
#[bench::case_realfft_3072(3_usize)]
#[bench::case_fft_4096(4_usize)]
#[bench::case_realfft_4096(5_usize)]
fn native_work_22(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_prediction(size: usize) -> NativeWork {
    let re = tone(size);
    let im: Vec<f32> = re.iter().rev().copied().collect();
    let mut history: [Vec<f32>; 4] = std::array::from_fn(|_| vec![0.0; size + 1]);
    NativeWork::new(move || {
        prediction_difference(
            [std::hint::black_box(&re), std::hint::black_box(&im)],
            std::hint::black_box(&mut history).each_mut().map(Vec::as_mut_slice),
        );
    })
}

#[metabench::benchmark(KERNEL_PREDICTION, "prediction_difference", "case", gungraun_setup = prepare_prediction)]
#[bench::case_1(0_usize)]
#[bench::case_3(1_usize)]
#[bench::case_17(2_usize)]
#[bench::case_64(3_usize)]
#[bench::case_513(4_usize)]
#[bench::case_1024(5_usize)]
fn native_work_23(work: NativeWork) -> NativeWork {
    work.run()
}

fn prepare_autocorrelation(size: usize) -> NativeWork {
    let region = pools();
    let frame = tone(size);
    let mut acf = Autocorrelation::new(NonZeroUsize::new(size).expect("size"), &region).expect("acf");
    let mut output = vec![0.0_f32; size];
    NativeWork::new(move || {
        acf.process(std::hint::black_box(&frame), &mut output);
    })
}

#[metabench::benchmark(KERNEL_AUTOCORRELATION, "autocorrelation", "case", gungraun_setup = prepare_autocorrelation)]
#[bench::case_process_64(0_usize)]
#[bench::case_process_128(1_usize)]
#[bench::case_process_256(2_usize)]
#[bench::case_process_512(3_usize)]
#[bench::case_process_1024(4_usize)]
#[bench::case_process_4096(5_usize)]
fn native_work_24(work: NativeWork) -> NativeWork {
    work.run()
}

