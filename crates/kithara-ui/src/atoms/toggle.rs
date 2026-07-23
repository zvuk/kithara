use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme,
    mouse::{self, Cursor},
    widget::{
        Space,
        canvas::{self, Action, Canvas, Frame, Geometry, Path, Stroke},
    },
};

use crate::{
    render::{ReadValue, Skin, UiEvent, theme::RenderPalette},
    skin::{CheckboxSkin, FrameSkin, ToggleSkin},
    widgets::{
        Widget,
        behavior::{ClickActivate, HoverState},
    },
};

#[derive(bon::Builder)]
pub(crate) struct Toggle<'path, 'value, 'data, 'skin> {
    path: &'path str,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Toggle<'_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        BinaryControl::builder()
            .path(self.path)
            .maybe_value(self.value)
            .skin(self.skin)
            .shape(Shape::Toggle {
                metrics: self.skin.toggle,
                active_border: self.skin.color(self.skin.toggle.active_frame.border),
                inactive_border: self.skin.color(self.skin.toggle.inactive_frame.border),
            })
            .build()
            .view()
    }
}

#[derive(bon::Builder)]
pub(crate) struct Checkbox<'path, 'value, 'data, 'skin> {
    path: &'path str,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Checkbox<'_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        BinaryControl::builder()
            .path(self.path)
            .maybe_value(self.value)
            .skin(self.skin)
            .shape(Shape::Checkbox {
                metrics: self.skin.checkbox,
                active_border: self.skin.color(self.skin.checkbox.active_frame.border),
                inactive_border: self.skin.color(self.skin.checkbox.inactive_frame.border),
            })
            .build()
            .view()
    }
}

#[derive(bon::Builder)]
struct BinaryControl<'path, 'value, 'data, 'skin> {
    path: &'path str,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
    shape: Shape,
}

impl<'a> Widget<'a> for BinaryControl<'_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Bool(active)) = self.value else {
            return Space::new().into();
        };
        Canvas::new(BinaryControlCanvas {
            active: *active,
            click: ClickActivate::builder()
                .path(self.path.to_owned())
                .hover(HoverState::new(mouse::Interaction::Pointer))
                .build(),
            palette: self.skin.palette,
            shape: self.shape,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
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

struct BinaryControlCanvas {
    active: bool,
    click: ClickActivate,
    palette: RenderPalette,
    shape: Shape,
}

impl canvas::Program<UiEvent> for BinaryControlCanvas {
    type State = ();

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
        self.click.mouse_interaction(bounds, cursor)
    }

    delegate::delegate! {
        to self.click {
            fn update(
                &self,
                state: &mut (),
                event: &Event,
                bounds: Rectangle,
                cursor: Cursor,
            ) -> Option<Action<UiEvent>>;
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
