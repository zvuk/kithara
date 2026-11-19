use kithara_config::Config;
use kithara_derive::Patch;

use crate::Pitch;

/// Controls applied by a renderer on its next quantum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Config, Patch)]
#[config(default, fields(value, get(copy)))]
pub struct WarpControls {
    /// Pitch apart from speed, where the backend supports it.
    #[config(live, builder(default), patch(skip))]
    pitch: Pitch,
}
