use bon::bon;
use kithara_config::Config;

use super::{Damping, Decay, Width};

/// Decay, high-frequency damping and stereo width of a wet reverb.
#[derive(Clone, Copy, Debug, Default, PartialEq, Config)]
#[config(builder(existing), fields(value, get(copy)))]
#[non_exhaustive]
pub struct ReverbConfig {
    decay: Decay,
    damping: Damping,
    width: Width,
}

#[bon]
impl ReverbConfig {
    #[builder(builder_type(vis = "pub"), start_fn(name = builder, vis = "pub"), finish_fn(vis = "pub"))]
    fn new(
        #[builder(default)] decay: Decay,
        #[builder(default)] damping: Damping,
        #[builder(default)] width: Width,
    ) -> Self {
        Self {
            decay,
            damping,
            width,
        }
    }
}
