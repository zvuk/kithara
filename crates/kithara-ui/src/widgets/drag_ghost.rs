use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    alignment::Vertical,
    mouse::{self, Cursor},
    widget::{
        canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke, Text as CanvasText},
        text,
    },
};
use num_traits::cast::AsPrimitive;

use super::Widget;
use crate::{
    render::{Skin, UiEvent, fonts},
    skin::DragSkin,
};

const ELLIPSIS: char = '\u{2026}';

/// Gap between the pointer and the box it carries, so the box trails the
/// pointer instead of sitting under it.
const POINTER_GAP: f32 = 12.0;

/// A canvas cannot measure text, so the label is cut to what fits: the mono
/// face this widget draws with advances that fraction of its size per glyph.
const MONO_ADVANCE: f32 = 0.6;

/// What the pointer is carrying, drawn at the pointer over everything the
/// layout lays out. It paints only: every event passes through it untouched,
/// and it claims no cursor of its own.
pub(crate) struct DragGhost {
    label: Option<String>,
    metrics: DragSkin,
    background: Color,
    border: Color,
    text_color: Color,
}

impl DragGhost {
    pub(crate) fn new(label: Option<String>, skin: &Skin) -> Self {
        let metrics = skin.drag;
        Self {
            label: label.map(|label| elide(&label, fitting_chars(metrics))),
            metrics,
            background: skin.color(metrics.background),
            border: skin.color(metrics.frame.border),
            text_color: skin.color(metrics.text.color),
        }
    }

    /// The box trails the pointer and stays inside the window.
    fn box_at(&self, pointer: Point, bounds: Rectangle) -> Rectangle {
        let size = Size::new(self.metrics.width, self.metrics.height);
        let x = (pointer.x + POINTER_GAP).min(bounds.width - size.width);
        let y = (pointer.y + POINTER_GAP).min(bounds.height - size.height);
        Rectangle::new(Point::new(x.max(0.0), y.max(0.0)), size)
    }
}

impl<'a> Widget<'a> for DragGhost {
    fn view(self) -> Element<'a, UiEvent> {
        Canvas::new(self)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

impl canvas::Program<UiEvent> for DragGhost {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        let (Some(label), Some(pointer)) = (self.label.as_ref(), cursor.position_in(bounds)) else {
            return Vec::new();
        };
        let mut frame = Frame::new(renderer, bounds.size());
        let ghost = self.box_at(pointer, bounds);
        let path = Path::rounded_rectangle(
            ghost.position(),
            ghost.size(),
            self.metrics.frame.radius.into(),
        );
        frame.fill(&path, self.background);
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(self.border)
                .with_width(self.metrics.frame.border_width),
        );
        frame.fill_text(CanvasText {
            content: label.clone(),
            position: Point::new(ghost.x + self.metrics.pad_x, ghost.center_y()),
            max_width: ghost.width - self.metrics.pad_x * 2.0,
            color: self.text_color,
            size: self.metrics.text.size.into(),
            font: fonts::family(self.metrics.text.font, self.metrics.text.weight),
            align_x: text::Alignment::Left,
            align_y: Vertical::Center,
            shaping: text::Shaping::Advanced,
            ..CanvasText::default()
        });
        vec![frame.into_geometry()]
    }

    /// The pointer moves without the host's state changing, so a ghost with
    /// something to carry asks for the frame that redraws it where the pointer
    /// now is. Empty-handed, it lets the move pass without a redraw.
    fn update(
        &self,
        _state: &mut (),
        event: &Event,
        _bounds: Rectangle,
        _cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        let moved = matches!(event, Event::Mouse(mouse::Event::CursorMoved { .. }));
        (moved && self.label.is_some()).then(Action::request_redraw)
    }

    fn mouse_interaction(
        &self,
        _state: &(),
        _bounds: Rectangle,
        _cursor: Cursor,
    ) -> mouse::Interaction {
        mouse::Interaction::None
    }
}

/// Glyphs the box holds between its paddings.
fn fitting_chars(metrics: DragSkin) -> usize {
    let inner = (metrics.width - metrics.pad_x * 2.0).max(0.0);
    (inner / (metrics.text.size * MONO_ADVANCE)).trunc().as_()
}

/// Keeps the label inside the box the skin authored for it.
fn elide(label: &str, max_chars: usize) -> String {
    let mut chars = label.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_none() {
        return head;
    }
    let mut elided: String = head.chars().take(max_chars.saturating_sub(1)).collect();
    elided.push(ELLIPSIS);
    elided
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn a_label_within_the_budget_is_left_alone() {
        assert_eq!(elide("Signal Path", 32), "Signal Path");
        assert_eq!(elide("exact", 5), "exact");
    }

    #[kithara::test]
    fn a_longer_label_ends_in_an_ellipsis() {
        assert_eq!(elide("Signal Path", 6), "Signa\u{2026}");
    }
}
