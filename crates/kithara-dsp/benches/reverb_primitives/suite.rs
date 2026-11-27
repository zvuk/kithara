use std::num::NonZeroUsize;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
use kithara_dsp::{
    delay::{DelayDiffuser, DelayLine},
    filter::OnePoleLowPass,
};
use kithara_test_utils::bufpool::pools;

fn tone(frames: usize) -> Vec<f32> {
    [0.25, -0.125, 0.375, -0.5, 0.0625, 0.75, -0.25]
        .into_iter()
        .cycle()
        .take(frames)
        .collect()
}

fn assert_bits(actual: &[f32], expected: &[f32]) {
    assert_eq!(
        actual.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
        expected.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
    );
}

fn routed_diffuse(c: &mut Criterion) {
    let mut group = c.benchmark_group("routes/delay_diffuse");
    for frames in [128, 512] {
        group.throughput(Throughput::Elements(
            u64::try_from(frames).expect("bench frames"),
        ));
        let input = tone(frames);
        let mut history = input.clone();
        history.rotate_left(3);
        let mut writes = vec![0.0; frames];
        for (name, kernel) in kithara_dsp::routes::delay_diffuse::routes() {
            group.bench_function(
                BenchmarkId::new("diffuse", format!("{name}_size_{frames}")),
                |b| {
                b.iter_batched_ref(
                    || input.clone(),
                    |samples| {
                        kernel(
                            std::hint::black_box(samples),
                            std::hint::black_box(&history),
                            std::hint::black_box(0.5),
                            std::hint::black_box(&mut writes),
                        )
                    },
                    BatchSize::NumIterations(64),
                );
            });
        }
    }
    group.finish();
}

fn routed_blend(c: &mut Criterion) {
    let mut group = c.benchmark_group("routes/delay_blend");
    for frames in [128, 512] {
        group.throughput(Throughput::Elements(
            u64::try_from(frames).expect("bench frames"),
        ));
        let newer = tone(frames);
        let mut older = newer.clone();
        older.rotate_left(1);
        let mut output = vec![0.0; frames];
        for (name, kernel) in kithara_dsp::routes::delay_blend::routes() {
            group.bench_function(
                BenchmarkId::new("blend", format!("{name}_size_{frames}")),
                |b| {
                b.iter(|| {
                    kernel(
                        std::hint::black_box(&newer),
                        std::hint::black_box(&older),
                        std::hint::black_box(0.37),
                        std::hint::black_box(&mut output),
                    )
                });
            });
        }
    }
    group.finish();
}

fn scalar_pole<const CHANNELS: usize>(
    planes: &mut [Vec<f32>; CHANNELS],
    state: &mut [f32; CHANNELS],
) {
    for (plane, memory) in planes.iter_mut().zip(state) {
        for sample in plane {
            *sample = 0.2_f32.mul_add(*memory, 0.8 * *sample);
            *memory = *sample;
        }
    }
}

fn one_pole<const CHANNELS: usize>(c: &mut Criterion) {
    let mut group = c.benchmark_group(format!("reverb_one_pole/{CHANNELS}ch"));
    for frames in [128, 512] {
        let template: [Vec<f32>; CHANNELS] = std::array::from_fn(|_| tone(frames));
        let mut expected = template.clone();
        let mut expected_state = [0.0; CHANNELS];
        scalar_pole(&mut expected, &mut expected_state);
        let mut actual = template.clone();
        let mut oracle = OnePoleLowPass::<CHANNELS>::new(0.2).expect("bench coefficient");
        oracle.process(&mut actual, 0..frames).expect("bench shape");
        for (actual, expected) in actual.iter().zip(&expected) {
            assert_bits(actual, expected);
        }
        for name in ["native", "process"] {
            let mut filter = OnePoleLowPass::<CHANNELS>::new(0.2).expect("bench coefficient");
            group.bench_function(BenchmarkId::new(name, format!("size_{frames}")), |b| {
                b.iter_batched_ref(
                    || template.clone(),
                    |planes| {
                        filter
                            .process(std::hint::black_box(planes), 0..frames)
                            .expect("bench shape");
                        std::hint::black_box(planes);
                    },
                    BatchSize::NumIterations(64),
                );
            });
        }
        let mut scalar_state = [0.0; CHANNELS];
        group.bench_function(BenchmarkId::new("scalar", format!("size_{frames}")), |b| {
            b.iter_batched_ref(
                || template.clone(),
                |planes| {
                    scalar_pole(std::hint::black_box(&mut *planes), &mut scalar_state);
                    std::hint::black_box((planes, &scalar_state));
                },
                BatchSize::NumIterations(64),
            );
        });
    }
    group.finish();
}

fn blocks(c: &mut Criterion) {
    let mut group = c.benchmark_group("reverb_blocks");
    let region = pools();
    for frames in [128, 512] {
        let input = tone(frames);
        let mut diffuser = DelayDiffuser::<2>::new(
            DelayLine::new(
                NonZeroUsize::new(2).expect("stereo"),
                NonZeroUsize::new(127).expect("delay"),
                &region,
            )
            .expect("history"),
            0.5,
        )
        .expect("diffuser");
        group.bench_function(BenchmarkId::new("diffuser_process_2ch", format!("size_{frames}")), |b| {
            b.iter_batched_ref(
                || [input.clone(), input.clone()],
                |planes| {
                    diffuser
                        .process(std::hint::black_box(planes), 0..frames)
                        .expect("shape");
                    std::hint::black_box(planes);
                },
                BatchSize::NumIterations(64),
            );
        });
        let mut line = DelayLine::new(
            NonZeroUsize::MIN,
            NonZeroUsize::new(2048).expect("capacity"),
            &region,
        )
        .expect("line");
        let history = tone(2048);
        line.write(&[history.as_slice()], 0..2048)
            .expect("fill history");
        let mut output = vec![0.0; frames];
        group.bench_function(BenchmarkId::new("delay_integer_scalar", format!("size_{frames}")), |b| {
            b.iter(|| {
                for (sample, output) in std::hint::black_box(history.get(1024..).expect("history span"))
                    .iter()
                    .zip(std::hint::black_box(&mut output))
                {
                    *output = *sample;
                }
                std::hint::black_box(&output);
            });
        });
        for (name, delay) in [("integer", 1024.0), ("fractional", 1024.37)] {
            let tap = line.tap(delay).expect("bench tap");
            group.bench_function(
                BenchmarkId::new(format!("delay_read/{name}"), format!("size_{frames}")),
                |b| {
                    b.iter(|| {
                        line.read(0, tap, std::hint::black_box(&mut output))
                            .expect("read span");
                        std::hint::black_box(&output);
                    });
                },
            );
        }
    }
    group.finish();
}

pub(crate) fn benchmarks(c: &mut Criterion) {
    routed_diffuse(c);
    routed_blend(c);
    blocks(c);
    one_pole::<2>(c);
    one_pole::<8>(c);
}

struct DiffusePrepared {
    samples: Vec<f32>,
    history: Vec<f32>,
    writes: Vec<f32>,
    route: usize,
}

fn diffuse_prepare(index: usize) -> DiffusePrepared {
    let frames = [128_usize, 512][index % 2];
    let input = tone(frames);
    let mut history = input.clone();
    history.rotate_left(3);
    DiffusePrepared {
        samples: input,
        history,
        writes: vec![0.0; frames],
        route: index / 2,
    }
}

#[metabench::benchmark(DELAY_DIFFUSE, "routes/delay_diffuse", "diffuse", gungraun_setup = diffuse_prepare)]
#[bench::simd_size_128(0_usize)]
#[bench::simd_size_512(1_usize)]
#[bench::scalar_size_128(2_usize)]
#[bench::scalar_size_512(3_usize)]
#[bench::entry_size_128(4_usize)]
#[bench::entry_size_512(5_usize)]
pub(crate) fn native_diffuse(mut prepared: DiffusePrepared) -> DiffusePrepared {
    let kernel = kithara_dsp::routes::delay_diffuse::routes()
        .nth(prepared.route)
        .expect("declared diffuse route")
        .1;
    kernel(
        std::hint::black_box(&mut prepared.samples),
        std::hint::black_box(&prepared.history),
        std::hint::black_box(0.5),
        std::hint::black_box(&mut prepared.writes),
    );
    prepared
}

struct BlendPrepared {
    newer: Vec<f32>,
    older: Vec<f32>,
    output: Vec<f32>,
    route: usize,
}

fn blend_prepare(index: usize) -> BlendPrepared {
    let frames = [128_usize, 512][index % 2];
    let newer = tone(frames);
    let mut older = newer.clone();
    older.rotate_left(1);
    BlendPrepared {
        newer,
        older,
        output: vec![0.0; frames],
        route: index / 2,
    }
}

#[metabench::benchmark(DELAY_BLEND, "routes/delay_blend", "blend", gungraun_setup = blend_prepare)]
#[bench::simd_size_128(0_usize)]
#[bench::simd_size_512(1_usize)]
#[bench::scalar_size_128(2_usize)]
#[bench::scalar_size_512(3_usize)]
#[bench::entry_size_128(4_usize)]
#[bench::entry_size_512(5_usize)]
pub(crate) fn native_blend(mut prepared: BlendPrepared) -> BlendPrepared {
    let kernel = kithara_dsp::routes::delay_blend::routes()
        .nth(prepared.route)
        .expect("declared blend route")
        .1;
    kernel(
        std::hint::black_box(&prepared.newer),
        std::hint::black_box(&prepared.older),
        std::hint::black_box(0.37),
        std::hint::black_box(&mut prepared.output),
    );
    prepared
}

struct OnePolePrepared<const CHANNELS: usize> {
    filter: OnePoleLowPass<CHANNELS>,
    planes: [Vec<f32>; CHANNELS],
    scalar_state: [f32; CHANNELS],
}

fn one_pole_prepare<const CHANNELS: usize>(frames: usize) -> OnePolePrepared<CHANNELS> {
    let template: [Vec<f32>; CHANNELS] = std::array::from_fn(|_| tone(frames));
    let mut expected = template.clone();
    let mut expected_state = [0.0; CHANNELS];
    scalar_pole(&mut expected, &mut expected_state);
    let mut actual = template.clone();
    let mut oracle = OnePoleLowPass::<CHANNELS>::new(0.2).expect("bench coefficient");
    oracle.process(&mut actual, 0..frames).expect("bench shape");
    for (actual, expected) in actual.iter().zip(&expected) {
        assert_bits(actual, expected);
    }
    OnePolePrepared {
        filter: OnePoleLowPass::<CHANNELS>::new(0.2).expect("bench coefficient"),
        planes: template,
        scalar_state: [0.0; CHANNELS],
    }
}

fn one_pole_2_prepare(frames: usize) -> OnePolePrepared<2> {
    one_pole_prepare(frames)
}

fn one_pole_8_prepare(frames: usize) -> OnePolePrepared<8> {
    one_pole_prepare(frames)
}

#[metabench::benchmark(ONE_POLE_2_NATIVE, "reverb_one_pole/2ch", "native", gungraun_setup = one_pole_2_prepare)]
#[bench::size_128(128_usize)]
#[bench::size_512(512_usize)]
pub(crate) fn native_one_pole_2_native(mut prepared: OnePolePrepared<2>) -> OnePolePrepared<2> {
    let frames = prepared.planes[0].len();
    prepared.filter.process(std::hint::black_box(&mut prepared.planes), 0..frames).expect("bench shape");
    std::hint::black_box(&prepared.planes);
    prepared
}

#[metabench::benchmark(ONE_POLE_2_PROCESS, "reverb_one_pole/2ch", "process", gungraun_setup = one_pole_2_prepare)]
#[bench::size_128(128_usize)]
#[bench::size_512(512_usize)]
pub(crate) fn native_one_pole_2_process(mut prepared: OnePolePrepared<2>) -> OnePolePrepared<2> {
    let frames = prepared.planes[0].len();
    prepared.filter.process(std::hint::black_box(&mut prepared.planes), 0..frames).expect("bench shape");
    std::hint::black_box(&prepared.planes);
    prepared
}

#[metabench::benchmark(ONE_POLE_2_SCALAR, "reverb_one_pole/2ch", "scalar", gungraun_setup = one_pole_2_prepare)]
#[bench::size_128(128_usize)]
#[bench::size_512(512_usize)]
pub(crate) fn native_one_pole_2_scalar(mut prepared: OnePolePrepared<2>) -> OnePolePrepared<2> {
    scalar_pole(std::hint::black_box(&mut prepared.planes), &mut prepared.scalar_state);
    std::hint::black_box(&prepared.planes);
    prepared
}

#[metabench::benchmark(ONE_POLE_8_NATIVE, "reverb_one_pole/8ch", "native", gungraun_setup = one_pole_8_prepare)]
#[bench::size_128(128_usize)]
#[bench::size_512(512_usize)]
pub(crate) fn native_one_pole_8_native(mut prepared: OnePolePrepared<8>) -> OnePolePrepared<8> {
    let frames = prepared.planes[0].len();
    prepared.filter.process(std::hint::black_box(&mut prepared.planes), 0..frames).expect("bench shape");
    std::hint::black_box(&prepared.planes);
    prepared
}

#[metabench::benchmark(ONE_POLE_8_PROCESS, "reverb_one_pole/8ch", "process", gungraun_setup = one_pole_8_prepare)]
#[bench::size_128(128_usize)]
#[bench::size_512(512_usize)]
pub(crate) fn native_one_pole_8_process(mut prepared: OnePolePrepared<8>) -> OnePolePrepared<8> {
    let frames = prepared.planes[0].len();
    prepared.filter.process(std::hint::black_box(&mut prepared.planes), 0..frames).expect("bench shape");
    std::hint::black_box(&prepared.planes);
    prepared
}

#[metabench::benchmark(ONE_POLE_8_SCALAR, "reverb_one_pole/8ch", "scalar", gungraun_setup = one_pole_8_prepare)]
#[bench::size_128(128_usize)]
#[bench::size_512(512_usize)]
pub(crate) fn native_one_pole_8_scalar(mut prepared: OnePolePrepared<8>) -> OnePolePrepared<8> {
    scalar_pole(std::hint::black_box(&mut prepared.planes), &mut prepared.scalar_state);
    std::hint::black_box(&prepared.planes);
    prepared
}

struct BlocksPrepared {
    diffuser: DelayDiffuser<2>,
    line: DelayLine,
    history: Vec<f32>,
    planes: [Vec<f32>; 2],
    output: Vec<f32>,
}

fn blocks_prepare(frames: usize) -> BlocksPrepared {
    let region = pools();
    let input = tone(frames);
    let diffuser = DelayDiffuser::<2>::new(
        DelayLine::new(
            NonZeroUsize::new(2).expect("stereo"),
            NonZeroUsize::new(127).expect("delay"),
            &region,
        ).expect("history"),
        0.5,
    ).expect("diffuser");
    let mut line = DelayLine::new(
        NonZeroUsize::MIN,
        NonZeroUsize::new(2048).expect("capacity"),
        &region,
    ).expect("line");
    let history = tone(2048);
    line.write(&[history.as_slice()], 0..2048).expect("fill history");
    BlocksPrepared {
        diffuser,
        line,
        history,
        planes: [input.clone(), input],
        output: vec![0.0; frames],
    }
}

#[metabench::benchmark(BLOCKS_DIFFUSER, "reverb_blocks", "diffuser_process_2ch", gungraun_setup = blocks_prepare)]
#[bench::size_128(128_usize)]
#[bench::size_512(512_usize)]
pub(crate) fn native_blocks_diffuser(mut prepared: BlocksPrepared) -> BlocksPrepared {
    let frames = prepared.output.len();
    prepared.diffuser.process(std::hint::black_box(&mut prepared.planes), 0..frames).expect("shape");
    prepared
}

#[metabench::benchmark(BLOCKS_SCALAR, "reverb_blocks", "delay_integer_scalar", gungraun_setup = blocks_prepare)]
#[bench::size_128(128_usize)]
#[bench::size_512(512_usize)]
pub(crate) fn native_blocks_scalar(mut prepared: BlocksPrepared) -> BlocksPrepared {
    for (sample, output) in std::hint::black_box(prepared.history.get(1024..).expect("history span"))
        .iter()
        .zip(std::hint::black_box(&mut prepared.output))
    {
        *output = *sample;
    }
    prepared
}

#[metabench::benchmark(BLOCKS_READ_INTEGER, "reverb_blocks", "delay_read/integer", gungraun_setup = blocks_prepare)]
#[bench::size_128(128_usize)]
#[bench::size_512(512_usize)]
pub(crate) fn native_blocks_read_integer(mut prepared: BlocksPrepared) -> BlocksPrepared {
    let tap = prepared.line.tap(1024.0).expect("bench tap");
    prepared.line.read(0, tap, std::hint::black_box(&mut prepared.output)).expect("read span");
    prepared
}

#[metabench::benchmark(BLOCKS_READ_FRACTIONAL, "reverb_blocks", "delay_read/fractional", gungraun_setup = blocks_prepare)]
#[bench::size_128(128_usize)]
#[bench::size_512(512_usize)]
pub(crate) fn native_blocks_read_fractional(mut prepared: BlocksPrepared) -> BlocksPrepared {
    let tap = prepared.line.tap(1024.37).expect("bench tap");
    prepared.line.read(0, tap, std::hint::black_box(&mut prepared.output)).expect("read span");
    prepared
}
