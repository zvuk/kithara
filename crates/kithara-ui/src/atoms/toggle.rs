use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Button, Cursor},
    widget::{
        Space,
        canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke},
    },
};

use crate::{
    render::{ControlAction, ReadValue, Skin, UiEvent, theme::RenderPalette},
    skin::{CheckboxSkin, FrameSkin, ToggleSkin},
};

pub(crate) fn toggle<'a>(
    path: &str,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    binary_control(
        path,
        value,
        skin,
        Shape::Toggle {
            metrics: skin.toggle,
            active_border: skin.color(skin.toggle.active_frame.border),
            inactive_border: skin.color(skin.toggle.inactive_frame.border),
        },
    )
}

pub(crate) fn checkbox<'a>(
    path: &str,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    binary_control(
        path,
        value,
        skin,
        Shape::Checkbox {
            metrics: skin.checkbox,
            active_border: skin.color(skin.checkbox.active_frame.border),
            inactive_border: skin.color(skin.checkbox.inactive_frame.border),
        },
    )
}

fn binary_control<'a>(
    path: &str,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
    shape: Shape,
) -> Element<'a, UiEvent> {
    let Some(ReadValue::Bool(active)) = value else {
        return Space::new().into();
    };

    Canvas::new(BinaryControl {
        active: *active,
        palette: skin.palette,
        path: path.to_owned(),
        shape,
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

#[derive(Clone, Copy)]
enum Shape {
    Toggle {
        metrics: ToggleSkin,
        active_border: Color,
        inactive_border: Color,
    },
    Checkbox {
        metrics: CheckboxSkin,
        active_border: Color,
        inactive_border: Color,
    },
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
            Shape::Toggle {
                metrics,
                active_border,
                inactive_border,
            } => draw_toggle(
                &mut frame,
                bounds,
                self.active,
                metrics,
                active_border,
                inactive_border,
                self.palette,
            ),
            Shape::Checkbox {
                metrics,
                active_border,
                inactive_border,
            } => draw_checkbox(
                &mut frame,
                bounds,
                self.active,
                metrics,
                active_border,
                inactive_border,
                self.palette,
            ),
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

fn draw_toggle(
    frame: &mut Frame,
    bounds: Rectangle,
    active: bool,
    metrics: ToggleSkin,
    active_border: Color,
    inactive_border: Color,
    palette: RenderPalette,
) {
    let frame_skin = if active {
        metrics.active_frame
    } else {
        metrics.inactive_frame
    };
    if active {
        fill_rounded(
            frame,
            Point::ORIGIN,
            bounds.size(),
            frame_skin.radius,
            palette.accent,
        );
    }
    draw_border(
        frame,
        bounds,
        frame_skin,
        if active {
            active_border
        } else {
            inactive_border
        },
    );
    let x = if active {
        bounds.width - metrics.thumb_inset - metrics.thumb_size
    } else {
        metrics.thumb_inset
    };
    fill_rounded(
        frame,
        Point::new(x, (bounds.height - metrics.thumb_size) / 2.0),
        Size::new(metrics.thumb_size, metrics.thumb_size),
        metrics.thumb_radius,
        if active {
            palette.bg_deep
        } else {
            palette.muted
        },
    );
}

fn draw_checkbox(
    frame: &mut Frame,
    bounds: Rectangle,
    active: bool,
    metrics: CheckboxSkin,
    active_border: Color,
    inactive_border: Color,
    palette: RenderPalette,
) {
    let frame_skin = if active {
        metrics.active_frame
    } else {
        metrics.inactive_frame
    };
    if active {
        fill_rounded(
            frame,
            Point::ORIGIN,
            bounds.size(),
            frame_skin.radius,
            palette.accent,
        );
    }
    draw_border(
        frame,
        bounds,
        frame_skin,
        if active {
            active_border
        } else {
            inactive_border
        },
    );
}

fn draw_border(frame: &mut Frame, bounds: Rectangle, skin: FrameSkin, color: Color) {
    if skin.border_width <= 0.0 {
        return;
    }
    let inset = skin.border_width / 2.0;
    let path = Path::rounded_rectangle(
        Point::new(inset, inset),
        Size::new(
            (bounds.width - skin.border_width).max(0.0),
            (bounds.height - skin.border_width).max(0.0),
        ),
        skin.radius.into(),
    );
    frame.stroke(
        &path,
        Stroke::default()
            .with_color(color)
            .with_width(skin.border_width),
    );
}

fn fill_rounded(frame: &mut Frame, point: Point, size: Size, radius: f32, color: Color) {
    frame.fill(&Path::rounded_rectangle(point, size, radius.into()), color);
}
