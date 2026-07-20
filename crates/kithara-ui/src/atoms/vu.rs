use iced::{
    Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Cursor},
    widget::{
        Space,
        canvas::{self, Action, Canvas, Frame, Geometry},
    },
};
use num_traits::cast::AsPrimitive;

use crate::{
    render::{ReadValue, Skin, StereoLevels, UiEvent, theme::RenderPalette},
    skin::VuVerticalSkin,
    widgets::{
        Widget,
        behavior::{HoverState, ScalarDrag, ScalarDragMode, ScalarDragState},
    },
};

#[derive(bon::Builder)]
pub(crate) struct VerticalVu<'path, 'value, 'data, 'skin> {
    path: &'path str,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for VerticalVu<'_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Stereo(levels)) = self.value else {
            return Space::new().into();
        };
        Canvas::new(VerticalVuCanvas {
            drag: ScalarDrag::builder()
                .path(self.path.to_owned())
                .mode(ScalarDragMode::Vertical)
                .hover(HoverState::new(mouse::Interaction::ResizingVertically))
                .build(),
            metrics: self.skin.vu_vertical,
            levels: *levels,
            palette: self.skin.palette,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

struct VerticalVuCanvas {
    drag: ScalarDrag,
    metrics: VuVerticalSkin,
    levels: StereoLevels,
    palette: RenderPalette,
}

impl canvas::Program<UiEvent> for VerticalVuCanvas {
    type State = ScalarDragState;

    fn draw(
        &self,
        _state: &ScalarDragState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), self.palette.bg_deep);
        draw_segments(&mut frame, bounds, self.levels, self.metrics, self.palette);
        draw_thumb(
            &mut frame,
            bounds,
            self.levels.volume,
            self.metrics,
            self.palette,
        );
        vec![frame.into_geometry()]
    }

    delegate::delegate! {
        to self.drag {
            fn update(
                &self,
                state: &mut ScalarDragState,
                event: &Event,
                bounds: Rectangle,
                cursor: Cursor,
            ) -> Option<Action<UiEvent>>;
            fn mouse_interaction(
                &self,
                state: &ScalarDragState,
                bounds: Rectangle,
                cursor: Cursor,
            ) -> mouse::Interaction;
        }
    }
}

fn draw_segments(
    frame: &mut Frame,
    bounds: Rectangle,
    levels: StereoLevels,
    metrics: VuVerticalSkin,
    palette: RenderPalette,
) {
    let step = metrics.segment_height + metrics.segment_gap;
    let count = ((bounds.height + metrics.segment_gap) / step).floor();
    if count <= 0.0 {
        return;
    }

    let count_usize: usize = count.as_();
    let level = levels.l.max(levels.r).clamp(0.0, 1.0);
    let lit = (level * count).round();
    let width = (bounds.width - metrics.segment_inset_x * 2.0).max(0.0);
    for index in 0..count_usize {
        let index: f32 = index.as_();
        let ratio = index / count;
        let color = if index >= lit {
            palette.bg_inset
        } else if ratio > metrics.danger_threshold {
            palette.danger
        } else if ratio > metrics.warning_threshold {
            palette.warning
        } else {
            palette.success
        };
        let y = bounds.height - metrics.segment_height - index * step;
        frame.fill_rectangle(
            Point::new(metrics.segment_inset_x, y),
            Size::new(width, metrics.segment_height),
            color,
        );
    }
}

fn draw_thumb(
    frame: &mut Frame,
    bounds: Rectangle,
    volume: f32,
    metrics: VuVerticalSkin,
    palette: RenderPalette,
) {
    let center_y = (1.0 - volume.clamp(0.0, 1.0)) * bounds.height;
    let y = center_y - metrics.thumb_height / 2.0;
    frame.fill_rectangle(
        Point::new(0.0, y),
        Size::new(bounds.width, metrics.thumb_height),
        palette.accent,
    );
    frame.fill_rectangle(
        Point::new(0.0, center_y - metrics.thumb_line_height / 2.0),
        Size::new(bounds.width, metrics.thumb_line_height),
        palette.bg_deep,
    );
}
