use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
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
    ticks: bool,
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
            ticks: self.ticks,
            levels: *levels,
            palette: self.skin.palette,
            thumb_color: self.skin.color(self.skin.vu_vertical.thumb_color),
            thumb_notch_color: self.skin.color(self.skin.vu_vertical.thumb_notch_color),
            tick_color: self.skin.color(self.skin.vu_vertical.tick_color),
            tick_center_color: self.skin.color(self.skin.vu_vertical.tick_center_color),
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

struct VerticalVuCanvas {
    drag: ScalarDrag,
    metrics: VuVerticalSkin,
    ticks: bool,
    levels: StereoLevels,
    palette: RenderPalette,
    thumb_color: Color,
    thumb_notch_color: Color,
    tick_color: Color,
    tick_center_color: Color,
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
        let canvas_bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            ..bounds
        };
        let fader = if self.ticks {
            fader_bounds(canvas_bounds, self.metrics.fader_width)
        } else {
            canvas_bounds
        };
        if self.ticks {
            draw_ticks(
                &mut frame,
                canvas_bounds,
                fader.x,
                self.metrics,
                self.tick_color,
                self.tick_center_color,
            );
        }
        draw_segments(&mut frame, fader, self.levels, self.metrics, self.palette);
        draw_thumb(
            &mut frame,
            fader,
            self.levels.volume,
            self.metrics,
            self.thumb_color,
            self.thumb_notch_color,
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

fn fader_bounds(bounds: Rectangle, width: f32) -> Rectangle {
    let width = width.clamp(0.0, bounds.width);
    Rectangle {
        x: bounds.x + bounds.width - width,
        width,
        ..bounds
    }
}

fn draw_ticks(
    frame: &mut Frame,
    bounds: Rectangle,
    fader_x: f32,
    metrics: VuVerticalSkin,
    color: Color,
    center_color: Color,
) {
    if metrics.tick_count < 2 {
        return;
    }
    let rail_right = (fader_x - metrics.tick_gap).max(bounds.x);
    let travel = (bounds.height - metrics.tick_height - metrics.tick_inset_y * 2.0).max(0.0);
    let last = metrics.tick_count - 1;
    let center = last / 2;
    for index in 0..metrics.tick_count {
        let is_center = metrics.tick_count % 2 == 1 && index == center;
        let width = if is_center {
            metrics.tick_center_width
        } else {
            metrics.tick_width
        }
        .min((rail_right - bounds.x).max(0.0));
        let index: f32 = index.as_();
        let last: f32 = last.as_();
        let y = bounds.y + metrics.tick_inset_y + index / last * travel;
        frame.fill_rectangle(
            Point::new(rail_right - width, y),
            Size::new(width, metrics.tick_height),
            if is_center { center_color } else { color },
        );
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
    let level = (levels.l.max(levels.r) * levels.volume).clamp(0.0, 1.0);
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
        let y = bounds.y + bounds.height - metrics.segment_height - index * step;
        frame.fill_rectangle(
            Point::new(bounds.x + metrics.segment_inset_x, y),
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
    color: Color,
    notch_color: Color,
) {
    let travel = (bounds.height - metrics.thumb_height).max(0.0);
    let y = bounds.y + ((1.0 - volume.clamp(0.0, 1.0)) * travel).round();
    frame.fill_rectangle(
        Point::new(bounds.x, y),
        Size::new(bounds.width, metrics.thumb_height),
        color,
    );
    if metrics.thumb_notch_offset < metrics.thumb_height {
        frame.fill_rectangle(
            Point::new(bounds.x, y + metrics.thumb_notch_offset),
            Size::new(bounds.width, 1.0),
            notch_color,
        );
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    /// The fader keeps its skin width and gives the rest of the box to the tick
    /// rail, so a wider meter moves the fader right instead of stretching it.
    #[kithara::test]
    fn the_fader_keeps_its_width_and_yields_the_rest_to_the_ticks() {
        let bare = fader_bounds(
            Rectangle {
                x: 3.0,
                width: 18.0,
                ..Rectangle::default()
            },
            18.0,
        );
        let with_ticks = fader_bounds(
            Rectangle {
                x: 3.0,
                width: 38.0,
                ..Rectangle::default()
            },
            18.0,
        );

        assert_eq!(bare.x, 3.0);
        assert_eq!(bare.width, 18.0);
        assert_eq!(with_ticks.x, 23.0);
        assert_eq!(with_ticks.width, 18.0);
    }
}
