use iced::{
    Background, Border, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme,
    widget::{
        Canvas, Row, Stack, button,
        button::Style as ButtonStyle,
        canvas::{self, Frame, Geometry, Path, Stroke},
        container,
        container::Style as ContainerStyle,
    },
};

use crate::{
    module::WindowControlsStyle,
    render::{Skin, UiEvent, WindowCommand},
    skin::{FrameSkin, WindowControlSkin},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct WindowControls<'skin> {
    style: WindowControlsStyle,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for WindowControls<'_> {
    fn view(self) -> Element<'a, UiEvent> {
        match self.skin.window.controls(self.style) {
            WindowControlSkin::Buttons {
                minus_icon_size,
                maximize_icon_size,
                close_icon_size,
                gap,
                padding,
            } => {
                let background = self.skin.color(self.skin.window.titlebar_background);
                container(
                    Row::with_children([
                        control_button(
                            Glyph::Minus,
                            minus_icon_size,
                            minus_icon_size,
                            self.skin.window.titlebar_height,
                            None,
                            WindowCommand::Minimize,
                            self.skin,
                        ),
                        control_button(
                            Glyph::Square,
                            maximize_icon_size,
                            maximize_icon_size,
                            self.skin.window.titlebar_height,
                            None,
                            WindowCommand::ToggleMaximize,
                            self.skin,
                        ),
                        control_button(
                            Glyph::Close,
                            close_icon_size,
                            close_icon_size,
                            self.skin.window.titlebar_height,
                            None,
                            WindowCommand::Close,
                            self.skin,
                        ),
                    ])
                    .spacing(gap)
                    .padding([0.0, padding])
                    .height(Length::Fixed(self.skin.window.titlebar_height)),
                )
                .style(move |_| ContainerStyle::default().background(Background::Color(background)))
                .into()
            }
            WindowControlSkin::Close {
                cell_size,
                icon_size,
                frame,
                divider,
            } => close_button(cell_size, icon_size, frame, divider, self.skin),
        }
    }
}

fn control_button<'a>(
    glyph: Glyph,
    icon_size: f32,
    width: f32,
    height: f32,
    frame: Option<FrameSkin>,
    command: WindowCommand,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let background = skin.color(skin.window.titlebar_background);
    let border = frame.map_or_else(Border::default, |frame| skin.border(frame));
    button(
        Canvas::new(WindowGlyph {
            glyph,
            size: icon_size,
            color: skin.color(skin.window.icon_color),
            hover_color: skin.color(skin.window.icon_hover_color),
            stroke_width: skin.window.icon_stroke_width,
        })
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .padding(0.0)
    .width(Length::Fixed(width))
    .height(Length::Fixed(height))
    .style(move |_theme: &Theme, _status| ButtonStyle {
        background: Some(Background::Color(background)),
        border,
        ..ButtonStyle::default()
    })
    .on_press(UiEvent::Window(command))
    .into()
}

fn close_button<'a>(
    cell_size: f32,
    icon_size: f32,
    frame: Option<FrameSkin>,
    divider: Option<(f32, crate::skin::ColorRole)>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let button = control_button(
        Glyph::Close,
        icon_size,
        cell_size,
        cell_size,
        frame,
        WindowCommand::Close,
        skin,
    );
    let Some((width, color)) = divider else {
        return button;
    };
    let divider = Canvas::new(Divider {
        width,
        color: skin.color(color),
    })
    .width(Length::Fixed(cell_size))
    .height(Length::Fixed(cell_size));
    Stack::with_children([button, divider.into()])
        .width(Length::Fixed(cell_size))
        .height(Length::Fixed(cell_size))
        .into()
}

#[derive(Clone, Copy)]
enum Glyph {
    Minus,
    Square,
    Close,
}

struct WindowGlyph {
    glyph: Glyph,
    size: f32,
    color: Color,
    hover_color: Color,
    stroke_width: f32,
}

impl<Message> canvas::Program<Message> for WindowGlyph {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let center = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let half = self.size / 2.0;
        let path = match self.glyph {
            Glyph::Minus => Path::line(
                Point::new(center.x - half, center.y),
                Point::new(center.x + half, center.y),
            ),
            Glyph::Square => Path::rectangle(
                Point::new(center.x - half, center.y - half),
                Size::new(self.size, self.size),
            ),
            Glyph::Close => Path::new(|builder| {
                builder.move_to(Point::new(center.x - half, center.y - half));
                builder.line_to(Point::new(center.x + half, center.y + half));
                builder.move_to(Point::new(center.x + half, center.y - half));
                builder.line_to(Point::new(center.x - half, center.y + half));
            }),
        };
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(if cursor.is_over(bounds) {
                    self.hover_color
                } else {
                    self.color
                })
                .with_width(self.stroke_width),
        );
        vec![frame.into_geometry()]
    }
}

struct Divider {
    width: f32,
    color: Color,
}

impl<Message> canvas::Program<Message> for Divider {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(
            Point::ORIGIN,
            Size::new(self.width, bounds.height),
            self.color,
        );
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::builtin;

    #[kithara::test]
    fn styles_select_their_skin_metrics() {
        let window = builtin::skin_doc().window;

        assert!(matches!(
            window.controls(WindowControlsStyle::Standard),
            WindowControlSkin::Buttons {
                minus_icon_size: 11.0,
                maximize_icon_size: 10.0,
                close_icon_size: 11.0,
                gap: 12.0,
                padding: 12.0,
            }
        ));
        assert!(matches!(
            window.controls(WindowControlsStyle::Compact),
            WindowControlSkin::Buttons {
                minus_icon_size: 10.0,
                maximize_icon_size: 9.0,
                close_icon_size: 10.0,
                gap: 10.0,
                padding: 10.0,
            }
        ));
        assert!(matches!(
            window.controls(WindowControlsStyle::CloseWide),
            WindowControlSkin::Close {
                cell_size: 32.0,
                icon_size: 11.0,
                divider: Some((1.0, _)),
                ..
            }
        ));
        assert!(matches!(
            window.controls(WindowControlsStyle::CloseMicro),
            WindowControlSkin::Close {
                cell_size: 28.0,
                icon_size: 10.0,
                frame: None,
                divider: None,
            }
        ));
        assert!(matches!(
            window.controls(WindowControlsStyle::CloseFramed),
            WindowControlSkin::Close {
                cell_size: 22.0,
                icon_size: 10.0,
                frame: Some(_),
                divider: None,
            }
        ));
    }
}
