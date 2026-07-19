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
    render::{ControlAction, ReadValue, Skin, StereoLevels, UiEvent, theme::RenderPalette},
    skin::VuStereoSkin,
};

pub(crate) fn view<'a>(
    path: &str,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let Some(ReadValue::Stereo(levels)) = value else {
        return Space::new().into();
    };

    Canvas::new(StereoMeter {
        metrics: skin.vu_stereo,
        levels: *levels,
        palette: skin.palette,
        path: path.to_owned(),
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

struct StereoMeter {
    metrics: VuStereoSkin,
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
            .zip([self.metrics.channel_l_y, self.metrics.channel_r_y])
        {
            draw_channel(&mut frame, y, level, self.metrics, self.palette);
        }

        let x = self.levels.volume.clamp(0.0, 1.0) * bounds.width;
        frame.fill_rectangle(
            Point::new(x, 0.0),
            Size::new(self.metrics.carriage_width, bounds.height),
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

fn draw_channel(
    frame: &mut Frame,
    y: f32,
    level: f32,
    metrics: VuStereoSkin,
    palette: RenderPalette,
) {
    let count: f32 = metrics.segment_count.as_();
    let lit = (level.clamp(0.0, 1.0) * count).round();

    for index in 0..metrics.segment_count {
        let index: f32 = index.as_();
        let x = index * (metrics.segment_width + metrics.segment_gap);
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
        frame.fill_rectangle(
            Point::new(x, y),
            Size::new(metrics.segment_width, metrics.segment_height),
            color,
        );
    }
}
