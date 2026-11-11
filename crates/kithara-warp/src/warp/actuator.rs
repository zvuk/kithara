use kithara_bufpool::{HasPool, PoolRegion};
use kithara_signal::AudioSpec;

use super::WarpConfig;
use crate::WarpRenderer;

/// Resident warp actuator around one decoded-audio source.
///
/// The wrapper remains present in identity and future synchronized modes. It
/// holds the configuration its worker-side renderer is built from.
#[derive(Debug, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct Warp<S> {
    config: WarpConfig,
    #[field(get, get_mut)]
    source: S,
}

impl<S> Warp<S> {
    /// Wraps `source` with the configuration its renderer is built from.
    #[must_use]
    pub fn new(source: S, config: &WarpConfig) -> Self {
        Self {
            source,
            config: config.clone(),
        }
    }

    /// Creates the worker-side renderer paired with this Warp facade.
    #[must_use]
    pub fn renderer<P>(&self, spec: AudioSpec, pools: PoolRegion<P>) -> WarpRenderer<P>
    where
        P: HasPool<f32>,
    {
        WarpRenderer::new(&self.config, spec, pools)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    #[kithara::test]
    fn source_access_delegates_to_the_resident_value() {
        let config = WarpConfig::builder().build();
        let mut warp = Warp::new(vec![1_u8], &config);

        warp.source_mut().push(2);

        assert_eq!(warp.source(), &[1, 2]);
    }
}
