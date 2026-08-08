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
    backends::font,
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::{HostLayer, Skin, UiEvent, WindowCommand},
    skin::DragSkin,
    text::TextContext,
};

/// What the pointer is carrying, drawn at the pointer over everything the
/// layout lays out. It paints only: every event passes through it untouched,
/// and it claims no cursor of its own.
pub(crate) struct DragGhost {
    background: Rgba,
    border: Rgba,
    text_color: Rgba,
    metrics: DragSkin,
    label: Option<String>,
}

impl DragGhost {
    const ELLIPSIS: char = '\u{2026}';

    /// A canvas cannot measure text, so the label is cut to what fits: the mono
    /// face this widget draws with advances that fraction of its size per glyph.
    const MONO_ADVANCE: f32 = 0.6;

    /// Gap between the pointer and the box it carries, so the box trails the
    /// pointer instead of sitting under it.
    const POINTER_GAP: f32 = 12.0;

    pub(crate) fn new(label: Option<String>, skin: &Skin) -> Self {
        let metrics = skin.drag;
        Self {
            label: label.map(|label| elide(&label, fitting_chars(metrics))),
            metrics,
            background: skin.rgba(metrics.background),
            border: skin.rgba(metrics.frame.border),
            text_color: skin.rgba(metrics.text.color),
        }
    }

    /// The box trails the pointer and stays inside the window.
    fn box_at(&self, pointer: Point, bounds: Rectangle) -> Rectangle {
        let size = Size::new(self.metrics.width, self.metrics.height);
        let x = (pointer.x + Self::POINTER_GAP).min(bounds.width - size.width);
        let y = (pointer.y + Self::POINTER_GAP).min(bounds.height - size.height);
        Rectangle::new(Point::new(x.max(0.0), y.max(0.0)), size)
    }

    pub(crate) fn layer(
        &self,
        pointer: Option<Pt>,
        bounds: Rect,
        text: &mut TextContext,
    ) -> HostLayer<WindowCommand> {
        let mut list = DrawListBuilder::default();
        if let (Some(label), Some(pointer)) = (self.label.as_ref(), pointer) {
            let pointer = Pt {
                x: pointer.x - bounds.x,
                y: pointer.y - bounds.y,
            };
            let size = (self.metrics.width, self.metrics.height);
            let ghost = Rect {
                h: size.1,
                w: size.0,
                x: (pointer.x + Self::POINTER_GAP)
                    .min(bounds.w - size.0)
                    .max(0.0),
                y: (pointer.y + Self::POINTER_GAP)
                    .min(bounds.h - size.1)
                    .max(0.0),
            };
            list.fill_rounded_rect(ghost, self.metrics.frame.radius, self.background);
            list.stroke_rounded_rect(
                ghost,
                self.metrics.frame.radius,
                self.border,
                self.metrics.frame.border_width,
            );
            let run = text.shape(
                label,
                self.metrics.text,
                Some((ghost.w - self.metrics.pad_x * 2.0).max(0.0)),
            );
            list.text(
                &run,
                label,
                Transform::translate(Pt {
                    x: ghost.x + self.metrics.pad_x,
                    y: ghost.y + (ghost.h - run.height()) / 2.0,
                }),
                self.text_color,
            );
        }
        HostLayer::new(bounds, list.finish(), Vec::new())
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
        frame.fill(&path, Color::from(self.background));
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(Color::from(self.border))
                .with_width(self.metrics.frame.border_width),
        );
        frame.fill_text(CanvasText {
            content: label.clone(),
            position: Point::new(ghost.x + self.metrics.pad_x, ghost.center_y()),
            max_width: ghost.width - self.metrics.pad_x * 2.0,
            color: Color::from(self.text_color),
            size: self.metrics.text.size.into(),
            font: font(self.metrics.text.font, self.metrics.text.weight),
            align_x: text::Alignment::Left,
            align_y: Vertical::Center,
            shaping: text::Shaping::Advanced,
            ..CanvasText::default()
        });
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &(),
        _bounds: Rectangle,
        _cursor: Cursor,
    ) -> mouse::Interaction {
        mouse::Interaction::None
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
}

/// Glyphs the box holds between its paddings.
fn fitting_chars(metrics: DragSkin) -> usize {
    let inner = (metrics.width - metrics.pad_x * 2.0).max(0.0);
    (inner / (metrics.text.size * DragGhost::MONO_ADVANCE))
        .trunc()
        .as_()
}

/// Keeps the label inside the box the skin authored for it.
fn elide(label: &str, max_chars: usize) -> String {
    let mut chars = label.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_none() {
        return head;
    }
    let mut elided: String = head.chars().take(max_chars.saturating_sub(1)).collect();
    elided.push(DragGhost::ELLIPSIS);
    elided
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::{DrawCmd, Geom, Pt},
        text::TextContext,
    };

    fn painted_box(layer: &HostLayer<WindowCommand>) -> Rect {
        let Some(DrawCmd::Fill {
            geom: Geom::RoundedRect { rect, .. },
            ..
        }) = layer.draw().commands().first()
        else {
            panic!("the carried label must start with its retained box fill");
        };
        *rect
    }

    #[kithara::test]
    fn a_label_within_the_budget_is_left_alone() {
        assert_eq!(elide("Signal Path", 32), "Signal Path");
        assert_eq!(elide("exact", 5), "exact");
    }

    #[kithara::test]
    fn a_longer_label_ends_in_an_ellipsis() {
        assert_eq!(elide("Signal Path", 6), "Signa\u{2026}");
    }

    #[kithara::test]
    fn the_retained_ghost_follows_the_pointer_and_has_no_hit_region() {
        let skin = builtin::skin();
        let ghost = DragGhost::new(Some("Signal Path".to_owned()), skin);
        let bounds = Rect {
            h: 80.0,
            w: 300.0,
            x: 10.0,
            y: 20.0,
        };
        let mut text = TextContext::from(skin.text_resources());

        let first = ghost.layer(Some(Pt { x: 20.0, y: 30.0 }), bounds, &mut text);
        let second = ghost.layer(Some(Pt { x: 40.0, y: 50.0 }), bounds, &mut text);

        let first_box = painted_box(&first);
        let second_box = painted_box(&second);
        assert_eq!(second_box.x - first_box.x, 20.0);
        assert_eq!(second_box.y - first_box.y, 20.0);
        assert!(first.hits().is_empty());
        assert!(second.hits().is_empty());
    }
}
