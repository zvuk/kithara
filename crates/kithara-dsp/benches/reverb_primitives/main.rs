#![forbid(unsafe_code)]

include!("suite.rs");

metabench::main!(
    criterion = benchmarks,
    benchmarks = [
        DELAY_DIFFUSE,
        DELAY_BLEND,
        ONE_POLE_2_NATIVE,
        ONE_POLE_2_PROCESS,
        ONE_POLE_2_SCALAR,
        ONE_POLE_8_NATIVE,
        ONE_POLE_8_PROCESS,
        ONE_POLE_8_SCALAR,
        BLOCKS_DIFFUSER,
        BLOCKS_SCALAR,
        BLOCKS_READ_INTEGER,
        BLOCKS_READ_FRACTIONAL,
    ],
    allocator = std::alloc::System,
);
