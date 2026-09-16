use kithara_platform::sync::Arc;
#[cfg(feature = "render")]
use {
    kithara_bufpool::{HasPool, PoolRegion},
    kithara_signal::AudioSpec,
};

use super::WarpConfig;
#[cfg(feature = "render")]
use super::WarpRenderer;
#[cfg(feature = "render")]
use crate::RenderReader;
use crate::{RegionPlanSlot, RenderPublisher, StretchControls};

/// Resident warp actuator around one decoded-audio source.
///
/// The wrapper remains present in identity and future synchronized modes. It
/// owns the live temporal controls that the playback layer composes into its
/// resident DSP path.
#[derive(Debug, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct Warp<S> {
    config: WarpConfig,
    publisher: Option<RenderPublisher>,
    #[cfg(feature = "render")]
    reader: RenderReader,
    /// Region plan of this resident item, installed by its deck.
    #[field(get, deref = false)]
    region_plan: Arc<RegionPlanSlot>,
    #[field(get, get_mut)]
    source: S,
}

impl<S> Warp<S> {
    /// Wraps `source` with the configured live temporal controls.
    #[must_use]
    pub fn new(source: S, config: &WarpConfig) -> Self {
        let publisher = RenderPublisher::default();
        #[cfg(feature = "render")]
        let reader = publisher.reader();
        Self {
            source,
            #[cfg(feature = "render")]
            reader,
            config: config.clone(),
            publisher: Some(publisher),
            region_plan: Arc::default(),
        }
    }

    /// Creates the worker-side renderer paired with this Warp facade.
    #[cfg(feature = "render")]
    #[must_use]
    pub fn renderer<P>(&self, spec: AudioSpec, pools: PoolRegion<P>) -> WarpRenderer<P>
    where
        P: HasPool<f32>,
    {
        WarpRenderer::new(
            &self.config,
            self.reader.clone(),
            spec,
            pools,
            Arc::clone(&self.region_plan),
        )
    }

    /// Live temporal controls shared with the resident Warp lane.
    #[must_use]
    pub fn stretch(&self) -> &Arc<StretchControls> {
        self.config.stretch()
    }

    /// Takes the sole callback-side publisher paired with this resident Warp.
    #[must_use]
    pub fn take_publisher(&mut self) -> Option<RenderPublisher> {
        self.publisher.take()
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

    #[kithara::test]
    fn controls_are_shared_with_the_resident_lane() {
        let stretch = StretchControls::new(1.0);
        let config = WarpConfig::builder().stretch(Arc::clone(&stretch)).build();
        let warp = Warp::new((), &config);

        warp.stretch().set_speed(1.25);

        assert!((stretch.speed() - 1.25).abs() < f32::EPSILON);
    }
}
