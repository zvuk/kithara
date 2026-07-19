use iced::{
    Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Button, Cursor},
    widget::{
        Space,
        canvas::{self, Action, Canvas, Frame, Geometry},
    },
};
use num_traits::cast::AsPrimitive;

use crate::{
    registry::{ControlKindDesc, ValueKind},
    render::{ControlAction, ReadValue, RenderPalette, StereoLevels, UiEvent},
    size::{Dim, SizeSpec},
};

struct Consts;

impl Consts {
    const DANGER_THRESHOLD: f32 = 0.83;
    const HEIGHT: f32 = 120.0;
    const SEGMENT_GAP: f32 = 2.0;
    const SEGMENT_HEIGHT: f32 = 4.0;
    const SEGMENT_INSET_X: f32 = 4.0;
    const THUMB_HEIGHT: f32 = 9.0;
    const THUMB_LINE_HEIGHT: f32 = 1.0;
    const WARNING_THRESHOLD: f32 = 0.66;
    const WIDTH: f32 = 18.0;
}

pub(crate) fn desc() -> ControlKindDesc {
    ControlKindDesc::new(Some(ValueKind::Stereo), Some(ValueKind::Scalar)).with_size(SizeSpec::new(
        Dim::Fixed(Consts::WIDTH),
        Dim::Fixed(Consts::HEIGHT),
    ))
}

pub(crate) fn view<'a>(
    path: &str,
    value: Option<&ReadValue<'_>>,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let Some(ReadValue::Stereo(levels)) = value else {
        return Space::new().into();
    };

    Canvas::new(VerticalVu {
        levels: *levels,
        palette,
        path: path.to_owned(),
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

struct VerticalVu {
    levels: StereoLevels,
    palette: RenderPalette,
    path: String,
}

#[derive(Default)]
struct DragState {
    active: bool,
}

impl canvas::Program<UiEvent> for VerticalVu {
    type State = DragState;

    fn update(
        &self,
        state: &mut DragState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) if cursor.is_over(bounds) => {
                state.active = true;
                scalar_action(&self.path, bounds, cursor)
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.active => {
                scalar_action(&self.path, bounds, cursor)
            }
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)) if state.active => {
                state.active = false;
                Some(Action::capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &DragState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), self.palette.bg_deep);
        draw_segments(&mut frame, bounds, self.levels, self.palette);
        draw_thumb(&mut frame, bounds, self.levels.volume, self.palette);
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &DragState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if state.active || cursor.is_over(bounds) {
            mouse::Interaction::ResizingVertically
        } else {
            mouse::Interaction::default()
        }
    }
}

fn scalar_action(path: &str, bounds: Rectangle, cursor: Cursor) -> Option<Action<UiEvent>> {
    if bounds.height <= 0.0 {
        return None;
    }
    cursor.position_from(bounds.position()).map(|position| {
        let volume = (1.0 - position.y / bounds.height).clamp(0.0, 1.0);
        Action::publish(UiEvent::Control {
            path: path.to_owned(),
            action: ControlAction::SetScalar(f64::from(volume)),
        })
        .and_capture()
    })
}

fn draw_segments(
    frame: &mut Frame,
    bounds: Rectangle,
    levels: StereoLevels,
    palette: RenderPalette,
) {
    let step = Consts::SEGMENT_HEIGHT + Consts::SEGMENT_GAP;
    let count = ((bounds.height + Consts::SEGMENT_GAP) / step).floor();
    if count <= 0.0 {
        return;
    }

    let count_usize: usize = count.as_();
    let level = levels.l.max(levels.r).clamp(0.0, 1.0);
    let lit = (level * count).round();
    let width = (bounds.width - Consts::SEGMENT_INSET_X * 2.0).max(0.0);
    for index in 0..count_usize {
        let index: f32 = index.as_();
        let ratio = index / count;
        let color = if index >= lit {
            palette.bg_inset
        } else if ratio > Consts::DANGER_THRESHOLD {
            palette.danger
        } else if ratio > Consts::WARNING_THRESHOLD {
            palette.warning
        } else {
            palette.success
        };
        let y = bounds.height - Consts::SEGMENT_HEIGHT - index * step;
        frame.fill_rectangle(
            Point::new(Consts::SEGMENT_INSET_X, y),
            Size::new(width, Consts::SEGMENT_HEIGHT),
            color,
        );
    }
}

fn draw_thumb(frame: &mut Frame, bounds: Rectangle, volume: f32, palette: RenderPalette) {
    let center_y = (1.0 - volume.clamp(0.0, 1.0)) * bounds.height;
    let y = center_y - Consts::THUMB_HEIGHT / 2.0;
    frame.fill_rectangle(
        Point::new(0.0, y),
        Size::new(bounds.width, Consts::THUMB_HEIGHT),
        palette.accent,
    );
    frame.fill_rectangle(
        Point::new(0.0, center_y - Consts::THUMB_LINE_HEIGHT / 2.0),
        Size::new(bounds.width, Consts::THUMB_LINE_HEIGHT),
        palette.bg_deep,
    );
}
