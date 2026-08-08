use kithara_platform::time::Instant;

use super::retained::Component;
use crate::{
    engine::model::{EngineEvent, Kind},
    interact::{CursorShape, Hit, Hover, Input, Outcome, recognizers::click},
};

pub(in crate::engine) struct SegmentedComponent {
    path: String,
    item_count: usize,
    hover: Hover,
}

impl SegmentedComponent {
    pub(super) fn new(path: String, item_count: usize) -> Self {
        Self {
            path,
            item_count,
            hover: Hover::new(CursorShape::Pointer),
        }
    }
}

impl Component for SegmentedComponent {
    fn path(&self) -> &str {
        &self.path
    }

    fn kind(&self) -> Kind {
        Kind::Segmented
    }

    fn handle(
        &mut self,
        input: Input<'_>,
        hit: &Hit,
        _index: Option<usize>,
        _now: Instant,
    ) -> (Outcome<EngineEvent>, Option<&'static str>) {
        let outcome = click::on_input(input, hit)
            .value()
            .and_then(|()| hit.uniform_horizontal_index(self.item_count))
            .map_or(Outcome::IGNORED, |index| {
                Outcome::set(EngineEvent::Index(index))
            });
        (outcome, None)
    }

    fn cursor(&self, hit: &Hit) -> CursorShape {
        if self.item_count == 0 {
            CursorShape::None
        } else {
            self.hover.cursor(false, hit)
        }
    }

    fn captures_pointer(&self) -> bool {
        false
    }
}
