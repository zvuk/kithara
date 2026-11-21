#![forbid(unsafe_code)]

use criterion::{criterion_group, criterion_main};

mod suite;

criterion_group!(benches, suite::benchmarks);
criterion_main!(benches);
