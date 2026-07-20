use std::ops::Range;

use kithara_audio::ServiceClass;

use super::{PlayerResource, ReadOutcome};
use crate::{api::TrackBinding, session::render::RenderContext};

pub(crate) struct PreparedElasticRenderer {
    _private: (),
}

impl PreparedElasticRenderer {
    pub(super) const fn decoded_frontier(&self) -> f64 {
        0.0
    }

    pub(super) fn set_service_class(&mut self, _class: ServiceClass) {}
}

impl PlayerResource {
    pub(crate) fn read_elastic(
        &mut self,
        _binding: &TrackBinding,
        _context: &RenderContext,
        _range: Range<usize>,
        _output: &mut [&mut [f32]],
    ) -> ReadOutcome {
        ReadOutcome::Failed
    }
}
