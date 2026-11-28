use kithara_config::Config;

use crate::{Band, consts};

/// Four-band peak scale and crossover boundaries.
#[derive(Config, Clone, Copy, Debug)]
#[config(default, builder(state_mod(vis = "pub")), fields(value, get(copy)))]
#[non_exhaustive]
pub struct AnalysisParams {
    #[config(builder(default = consts::BAND_GAIN))]
    band_gain: [f32; Band::COUNT],
    /// Geometric means of the adjacent four-band EQ centres, in Hz.
    #[config(builder(default = consts::BAND_EDGES_HZ))]
    band_edges_hz: [f64; 3],
}
