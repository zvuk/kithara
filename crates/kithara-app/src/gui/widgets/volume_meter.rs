use iced::{
    Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Button, Cursor},
    widget::canvas::{self, Action, Canvas, Frame, Geometry, Stroke},
};
use num_traits::cast::AsPrimitive;

use crate::{
    gui::{
        message::Message,
        modular::{ControlAction, ModularMsg},
        tokens::volume,
    },
    theme::gui::GuiPalette,
};

/// Interactive volume strip whose lit segments currently follow the volume.
/// A live output level can later drive a separate pulsing overlay while volume
/// remains the stable base fill and the value published by pointer input.
struct SegmentedVolume {
    palette: GuiPalette,
    path: String,
    volume: f32,
}

#[derive(Default)]
struct DragState {
    active: bool,
}

impl canvas::Program<Message> for SegmentedVolume {
    type State = DragState;

    fn update(
        &self,
        state: &mut DragState,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Message>> {
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
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), self.palette.canvas.bg_deep);
        draw_segments(&mut frame, bounds, self.volume, self.palette);
        draw_border(&mut frame, bounds, self.palette.canvas.line);
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

pub(crate) fn view(palette: GuiPalette, path: String, value: f64) -> Element<'static, Message> {
    Canvas::new(SegmentedVolume {
        palette,
        path,
        volume: value.clamp(0.0, 1.0).as_(),
    })
    .width(Length::Fill)
    .height(Length::Fixed(volume::STRIP_HEIGHT))
    .into()
}

fn scalar_action(path: &str, bounds: Rectangle, cursor: Cursor) -> Option<Action<Message>> {
    if bounds.width <= 0.0 {
        return None;
    }
    cursor.position_from(bounds.position()).map(|position| {
        let value = (position.x / bounds.width).clamp(0.0, 1.0);
        Action::publish(Message::Modular(ModularMsg::Control {
            path: path.to_owned(),
            action: ControlAction::SetScalar(f64::from(value)),
        }))
        .and_capture()
    })
}

fn draw_segments(frame: &mut Frame, bounds: Rectangle, level: f32, palette: GuiPalette) {
    let count: f32 = volume::SEGMENT_COUNT.as_();
    let gap_width = volume::SEGMENT_GAP * (count - 1.0);
    let content_width = (bounds.width - volume::STRIP_PADDING * 2.0).max(0.0);
    let segment_width = ((content_width - gap_width) / count).max(0.0);
    if segment_width <= 0.0 {
        return;
    }

    let segment_height =
        volume::SEGMENT_HEIGHT.min((bounds.height - volume::STRIP_PADDING * 2.0).max(0.0));
    let y = (bounds.height - segment_height) / 2.0;
    let lit = (level.clamp(0.0, 1.0) * count).round();
    for index in 0..volume::SEGMENT_COUNT {
        let index: f32 = index.as_();
        let x = volume::STRIP_PADDING + index * (segment_width + volume::SEGMENT_GAP);
        frame.fill_rectangle(
            Point::new(x, y),
            Size::new(segment_width, segment_height),
            if index < lit {
                palette.success
            } else {
                palette.canvas.bg_panel
            },
        );
    }
}

fn draw_border(frame: &mut Frame, bounds: Rectangle, color: iced::Color) {
    let inset = volume::BORDER_WIDTH / 2.0;
    let width = (bounds.width - volume::BORDER_WIDTH).max(0.0);
    let height = (bounds.height - volume::BORDER_WIDTH).max(0.0);
    frame.stroke_rectangle(
        Point::new(inset, inset),
        Size::new(width, height),
        Stroke::default()
            .with_color(color)
            .with_width(volume::BORDER_WIDTH),
    );
}
