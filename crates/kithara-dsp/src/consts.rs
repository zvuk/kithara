/// Fewest factors of two in an FFT length vDSP's real DFT builds.
pub(crate) const FFT_MIN_TWOS: u32 = 4;
/// Hann window: `w[n] = A0 − A0·cos(2πn / (N − 1))`.
#[cfg(feature = "spectrum")]
pub(crate) const HANN_A0: f32 = 0.5;

/// Frames one iteration of the strided copies moves. A loop that moves one
/// sample per iteration runs at half speed whenever it straddles a 4096-byte
/// page, and `opt-level = "z"` neither unrolls nor aligns it; four samples
/// per iteration amortize that fetch and roughly halve the cost everywhere.
pub(crate) const LANES: usize = 4;

/// Interpolation positions are `f32`: every frame index below `2²⁴` is exact.
pub(crate) const MAX_WINDOW: u32 = 1 << 24;

/// Frames per biquad settle pass; the scratch planes hold this many.
pub(crate) const SETTLE_CHUNK: usize = 64;
/// `log2` of the residue a biquad transient decays to before it counts as
/// settled.
pub(crate) const SETTLE_FLOOR_LOG2: f64 = -24.0;
/// Biquad input and output peaks at or below this reset the state.
pub(crate) const SILENT_PEAK: f32 = 1.0e-9;
/// Frames a biquad section remembers: a quiet call proves the state silent
/// only when it covers them.
pub(crate) const SECTION_MEMORY: usize = 2;
