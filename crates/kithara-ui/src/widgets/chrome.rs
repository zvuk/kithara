use iced::{
    Background, Border, Element, Length, Point, Rectangle, Renderer, Size, Theme,
    widget::{
        Canvas, Stack,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        canvas::{self, Frame, Geometry},
        container,
        container::Style as ContainerStyle,
        text_input::{Status as TextInputStatus, Style as TextInputStyle},
    },
};

use crate::render::Skin;

/// Framed module shell shared by renderer and application surfaces.
#[derive(bon::Builder)]
#[non_exhaustive]
pub struct ModuleChrome<'a, Content> {
    content: Content,
    skin: &'a Skin,
}

impl<Content> ModuleChrome<'_, Content> {
    pub fn view<'a, Message>(self) -> Element<'a, Message>
    where
        Message: 'a,
        Content: Into<Element<'a, Message>>,
    {
        let palette = self.skin.palette;
        let border = self.skin.border(self.skin.chrome.frame);
        let body = container(self.content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| module_style(palette, border));
        let ticks = Canvas::new(CornerTicks {
            color: self.skin.color(self.skin.chrome.corner_color),
            size: self.skin.chrome.corner_size,
            width: self.skin.chrome.corner_width,
            offset: self.skin.chrome.corner_offset,
        })
        .width(Length::Fill)
        .height(Length::Fill);

        Stack::with_children([body.into(), ticks.into()])
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

pub fn secondary_button_style(
    skin: &Skin,
) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle + 'static {
    let palette = skin.palette;
    let border = skin.border(skin.chrome.secondary_frame);
    move |_theme, status| {
        let background = match status {
            ButtonStatus::Hovered => palette.bg_panel_2,
            ButtonStatus::Pressed => palette.accent_soft,
            ButtonStatus::Active | ButtonStatus::Disabled => palette.bg_panel,
        };
        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color: palette.text,
            border,
            ..ButtonStyle::default()
        }
    }
}

pub(crate) fn text_input_style(
    skin: &Skin,
) -> impl Fn(&Theme, TextInputStatus) -> TextInputStyle + 'static {
    let palette = skin.palette;
    let metrics = skin.text_input;
    let border_color = skin.color(metrics.border);
    move |_theme, status| TextInputStyle {
        background: Background::Color(palette.bg_inset),
        border: Border {
            width: if matches!(status, TextInputStatus::Focused { .. }) {
                metrics.border_width
            } else {
                metrics.idle_border_width
            },
            color: border_color,
            radius: metrics.radius.into(),
        },
        icon: palette.muted,
        placeholder: palette.muted,
        value: palette.text,
        selection: palette.accent_soft,
    }
}

fn module_style(palette: crate::render::theme::RenderPalette, border: Border) -> ContainerStyle {
    ContainerStyle::default()
        .background(Background::Color(palette.bg_panel))
        .border(border)
}

struct CornerTicks {
    color: iced::Color,
    size: f32,
    width: f32,
    offset: f32,
}

impl<Message> canvas::Program<Message> for CornerTicks {
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
        let right = (bounds.width - self.offset - self.width).max(0.0);
        let bottom = (bounds.height - self.offset - self.width).max(0.0);
        let right_tick = (bounds.width - self.offset - self.size).max(0.0);
        let bottom_tick = (bounds.height - self.offset - self.size).max(0.0);

        frame.fill_rectangle(
            Point::new(self.offset, self.offset),
            Size::new(self.size, self.width),
            self.color,
        );
        frame.fill_rectangle(
            Point::new(self.offset, self.offset),
            Size::new(self.width, self.size),
            self.color,
        );
        frame.fill_rectangle(
            Point::new(right_tick, bottom),
            Size::new(self.size, self.width),
            self.color,
        );
        frame.fill_rectangle(
            Point::new(right, bottom_tick),
            Size::new(self.width, self.size),
            self.color,
        );

        vec![frame.into_geometry()]
    }
}
