use iced::{
    Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Button, Cursor},
    widget::{
        Space,
        canvas::{self, Action, Canvas, Frame, Geometry},
    },
};
use num_traits::cast::AsPrimitive;

use crate::render::{ControlAction, ReadValue, RenderPalette, StereoLevels, UiEvent};

struct Consts;

impl Consts {
    const CARRIAGE_WIDTH: f32 = 2.0;
    const CHANNEL_COUNT: usize = 2;
    const DANGER_THRESHOLD: f32 = 0.83;
    const ROW_Y: [f32; Self::CHANNEL_COUNT] = [2.0, 14.0];
    const SEGMENT_COUNT: usize = 16;
    const SEGMENT_GAP: f32 = 1.0;
    const SEGMENT_HEIGHT: f32 = 8.0;
    const SEGMENT_WIDTH: f32 = 3.0;
    const WARNING_THRESHOLD: f32 = 0.66;
}

pub(crate) fn view<'a>(
    path: &str,
    value: Option<&ReadValue<'_>>,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let Some(ReadValue::Stereo(levels)) = value else {
        return Space::new().into();
    };

    Canvas::new(StereoMeter {
        levels: *levels,
        palette,
        path: path.to_owned(),
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

struct StereoMeter {
    levels: StereoLevels,
    palette: RenderPalette,
    path: String,
}

#[derive(Default)]
struct DragState {
    active: bool,
}

impl canvas::Program<UiEvent> for StereoMeter {
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

        for (level, y) in [self.levels.l, self.levels.r]
            .into_iter()
            .zip(Consts::ROW_Y)
        {
            draw_channel(&mut frame, y, level, self.palette);
        }

        let x = self.levels.volume.clamp(0.0, 1.0) * bounds.width;
        frame.fill_rectangle(
            Point::new(x, 0.0),
            Size::new(Consts::CARRIAGE_WIDTH, bounds.height),
            self.palette.accent,
        );
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &DragState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if state.active || cursor.is_over(bounds) {
            mouse::Interaction::ResizingHorizontally
        } else {
            mouse::Interaction::default()
        }
    }
}

fn scalar_action(path: &str, bounds: Rectangle, cursor: Cursor) -> Option<Action<UiEvent>> {
    if bounds.width <= 0.0 {
        return None;
    }
    cursor.position_from(bounds.position()).map(|position| {
        let volume = (position.x / bounds.width).clamp(0.0, 1.0);
        Action::publish(UiEvent::Control {
            path: path.to_owned(),
            action: ControlAction::SetScalar(f64::from(volume)),
        })
        .and_capture()
    })
}

fn draw_channel(frame: &mut Frame, y: f32, level: f32, palette: RenderPalette) {
    let count: f32 = Consts::SEGMENT_COUNT.as_();
    let lit = (level.clamp(0.0, 1.0) * count).round();

    for index in 0..Consts::SEGMENT_COUNT {
        let index: f32 = index.as_();
        let x = index * (Consts::SEGMENT_WIDTH + Consts::SEGMENT_GAP);
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
        frame.fill_rectangle(
            Point::new(x, y),
            Size::new(Consts::SEGMENT_WIDTH, Consts::SEGMENT_HEIGHT),
            color,
        );
    }
}
