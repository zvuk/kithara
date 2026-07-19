use iced::{
    Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Button, Cursor},
    widget::{
        Space,
        canvas::{self, Action, Canvas, Frame, Geometry, Stroke},
    },
};

use crate::render::{ControlAction, ReadValue, RenderPalette, UiEvent};

struct Consts;

impl Consts {
    const BORDER_WIDTH: f32 = 1.0;
    const THUMB_INSET: f32 = 2.0;
    const THUMB_SIZE: f32 = 9.0;
}

pub(crate) fn toggle<'a>(
    path: &str,
    value: Option<&ReadValue<'_>>,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    binary_control(path, value, palette, Shape::Toggle)
}

pub(crate) fn checkbox<'a>(
    path: &str,
    value: Option<&ReadValue<'_>>,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    binary_control(path, value, palette, Shape::Checkbox)
}

fn binary_control<'a>(
    path: &str,
    value: Option<&ReadValue<'_>>,
    palette: RenderPalette,
    shape: Shape,
) -> Element<'a, UiEvent> {
    let Some(ReadValue::Bool(active)) = value else {
        return Space::new().into();
    };

    Canvas::new(BinaryControl {
        active: *active,
        palette,
        path: path.to_owned(),
        shape,
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

#[derive(Clone, Copy)]
enum Shape {
    Toggle,
    Checkbox,
}

struct BinaryControl {
    active: bool,
    palette: RenderPalette,
    path: String,
    shape: Shape,
}

impl canvas::Program<UiEvent> for BinaryControl {
    type State = ();

    fn update(
        &self,
        _state: &mut (),
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<UiEvent>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) if cursor.is_over(bounds) => {
                Some(
                    Action::publish(UiEvent::Control {
                        path: self.path.clone(),
                        action: ControlAction::Activate,
                    })
                    .and_capture(),
                )
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        match self.shape {
            Shape::Toggle => draw_toggle(&mut frame, bounds, self.active, self.palette),
            Shape::Checkbox => draw_checkbox(&mut frame, bounds, self.active, self.palette),
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &(),
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

fn draw_toggle(frame: &mut Frame, bounds: Rectangle, active: bool, palette: RenderPalette) {
    if active {
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), palette.accent);
    } else {
        draw_border(frame, bounds, palette.muted);
    }
    let x = if active {
        bounds.width - Consts::THUMB_INSET - Consts::THUMB_SIZE
    } else {
        Consts::THUMB_INSET
    };
    frame.fill_rectangle(
        Point::new(x, (bounds.height - Consts::THUMB_SIZE) / 2.0),
        Size::new(Consts::THUMB_SIZE, Consts::THUMB_SIZE),
        if active {
            palette.bg_deep
        } else {
            palette.muted
        },
    );
}

fn draw_checkbox(frame: &mut Frame, bounds: Rectangle, active: bool, palette: RenderPalette) {
    if active {
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), palette.accent);
        draw_border(frame, bounds, palette.line);
    } else {
        draw_border(frame, bounds, palette.muted);
    }
}

fn draw_border(frame: &mut Frame, bounds: Rectangle, color: iced::Color) {
    let inset = Consts::BORDER_WIDTH / 2.0;
    frame.stroke_rectangle(
        Point::new(inset, inset),
        Size::new(
            (bounds.width - Consts::BORDER_WIDTH).max(0.0),
            (bounds.height - Consts::BORDER_WIDTH).max(0.0),
        ),
        Stroke::default()
            .with_color(color)
            .with_width(Consts::BORDER_WIDTH),
    );
}
