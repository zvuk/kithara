use kithara_platform::time::Instant;
use num_traits::ToPrimitive;

use super::retained::Component;
use crate::{
    draw::Rect,
    engine::model::{EngineEvent, Kind, ScrollConfig},
    interact::{CursorShape, Hit, Input, Outcome, Scroll, ScrollAxis, recognizers::click},
};

const LINE_STEP_PX: f32 = 60.0;

#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct ScrollState {
    config: ScrollConfig,
    #[field(get, vis = "pub(crate)")]
    offset: f32,
    viewport_extent: f32,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self::new(ScrollConfig::plain(ScrollAxis::Vertical, 0.0))
    }
}

impl ScrollState {
    pub(crate) const fn new(config: ScrollConfig) -> Self {
        Self {
            config,
            offset: 0.0,
            viewport_extent: 0.0,
        }
    }

    pub(crate) fn reconcile(&mut self, config: ScrollConfig) {
        self.config = config;
        self.clamp_offset();
    }

    pub(crate) fn sync_offset(&mut self, offset: f32) {
        self.offset = offset.clamp(0.0, self.max_offset());
    }

    pub(crate) fn set_viewport(&mut self, extent: f32) {
        self.viewport_extent = extent.max(0.0);
        self.clamp_offset();
    }

    pub(crate) fn handle(&mut self, input: Input<'_>, hit: &Hit) -> Outcome<usize> {
        if let Input::Wheel(scroll) = input {
            return self.wheel(scroll, hit);
        }
        if click::on_input(input, hit) == Outcome::IGNORED {
            return Outcome::IGNORED;
        }
        self.row_at(hit).map_or(Outcome::IGNORED, Outcome::set)
    }

    fn wheel(&mut self, scroll: Scroll, hit: &Hit) -> Outcome<usize> {
        if !hit.over() {
            return Outcome::IGNORED;
        }
        let area = hit.area();
        self.set_viewport(match self.config.axis() {
            ScrollAxis::Horizontal => area.w,
            ScrollAxis::Vertical => area.h,
        });
        let delta = scroll.delta(self.config.axis());
        if delta == 0.0 {
            return Outcome::IGNORED;
        }
        let delta = if scroll.is_pixels() {
            delta
        } else {
            delta * LINE_STEP_PX
        };
        let next = (self.offset - delta).clamp(0.0, self.max_offset());
        if next == self.offset {
            return Outcome::IGNORED;
        }
        self.offset = next;
        Outcome::captured()
    }

    fn row_at(&self, hit: &Hit) -> Option<usize> {
        let point = hit.inside()?;
        let items = self.config.item_layout()?;
        if self.config.axis() != ScrollAxis::Vertical || items.extent <= 0.0 || items.size <= 0.0 {
            return None;
        }
        let row_right = hit.area().x + (hit.area().w - items.cross_inset).max(0.0);
        if self.max_offset() > 0.0 && point.x >= row_right {
            return None;
        }
        let y = point.y - hit.area().y + self.offset;
        if y < 0.0 {
            return None;
        }
        let index = (y / items.extent).floor().to_usize()?;
        if index >= items.count || y - index.to_f32()? * items.extent >= items.size {
            return None;
        }
        Some(index)
    }

    fn clamp_offset(&mut self) {
        self.offset = self.offset.clamp(0.0, self.max_offset());
    }

    fn max_offset(&self) -> f32 {
        (self.config.content_extent().max(0.0) - self.viewport_extent).max(0.0)
    }
}

pub(in crate::engine) struct ScrollComponent {
    path: String,
    state: ScrollState,
}

impl ScrollComponent {
    pub(super) fn new(path: String, config: ScrollConfig) -> Self {
        Self {
            path,
            state: ScrollState::new(config),
        }
    }

    pub(super) fn reconcile(mut self, next: Self) -> Self {
        self.path = next.path;
        self.state.reconcile(next.state.config);
        self
    }

    pub(super) fn offset(&self) -> f32 {
        self.state.offset()
    }

    pub(super) fn set_viewport(&mut self, area: Rect) {
        self.state.set_viewport(match self.state.config.axis() {
            ScrollAxis::Horizontal => area.w,
            ScrollAxis::Vertical => area.h,
        });
    }
}

impl Component for ScrollComponent {
    fn path(&self) -> &str {
        &self.path
    }

    fn kind(&self) -> Kind {
        Kind::Scroll
    }

    fn handle(
        &mut self,
        input: Input<'_>,
        hit: &Hit,
        _index: Option<usize>,
        _now: Instant,
    ) -> (Outcome<EngineEvent>, Option<&'static str>) {
        (self.state.handle(input, hit).map(EngineEvent::Index), None)
    }

    fn cursor(&self, hit: &Hit) -> CursorShape {
        if self.state.row_at(hit).is_some() {
            CursorShape::Pointer
        } else {
            CursorShape::None
        }
    }

    fn captures_pointer(&self) -> bool {
        false
    }
}
