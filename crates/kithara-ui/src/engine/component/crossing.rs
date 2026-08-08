use kithara_platform::time::Instant;

use super::retained::Component;
use crate::{
    engine::model::{EngineEvent, Kind},
    interact::{CursorShape, Hit, Input, Outcome, recognizers::Crossing},
};

pub(in crate::engine) struct CrossingComponent {
    crossing: Crossing,
    path: String,
}

impl CrossingComponent {
    pub(super) fn new(path: String) -> Self {
        Self {
            crossing: Crossing::default(),
            path,
        }
    }
}

impl Component for CrossingComponent {
    fn path(&self) -> &str {
        &self.path
    }

    fn kind(&self) -> Kind {
        Kind::Crossing
    }

    fn handle(
        &mut self,
        input: Input<'_>,
        hit: &Hit,
        _index: Option<usize>,
        _now: Instant,
    ) -> (Outcome<EngineEvent>, Option<&'static str>) {
        (
            self.crossing
                .on_input(input, hit)
                .map(EngineEvent::Crossing),
            None,
        )
    }

    fn cursor(&self, _hit: &Hit) -> CursorShape {
        CursorShape::None
    }

    fn captures_pointer(&self) -> bool {
        false
    }
}
