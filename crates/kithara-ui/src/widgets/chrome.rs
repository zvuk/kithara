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

use crate::render::RenderPalette;

struct Consts;

impl Consts {
    const BORDER_WIDTH: f32 = 1.0;
    const CORNER_TICK_SIZE: f32 = 10.0;
    const CORNER_TICK_WIDTH: f32 = 2.0;
}

pub fn module_chrome<'a, Message: 'a, C: Into<Element<'a, Message>>>(
    content: C,
    palette: RenderPalette,
) -> Element<'a, Message> {
    let body = container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| module_style(palette));
    let ticks = Canvas::new(CornerTicks {
        color: palette.accent,
    })
    .width(Length::Fill)
    .height(Length::Fill);

    Stack::with_children([body.into(), ticks.into()])
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

pub fn secondary_button_style(
    palette: RenderPalette,
) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle {
    move |_theme, status| {
        let background = match status {
            ButtonStatus::Hovered => palette.bg_panel_2,
            ButtonStatus::Pressed => palette.accent_soft,
            ButtonStatus::Active | ButtonStatus::Disabled => palette.bg_panel,
        };
        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color: palette.text,
            border: Border::default()
                .width(Consts::BORDER_WIDTH)
                .color(palette.line),
            ..ButtonStyle::default()
        }
    }
}

pub(crate) fn text_input_style(
    palette: RenderPalette,
) -> impl Fn(&Theme, TextInputStatus) -> TextInputStyle {
    move |_theme, status| TextInputStyle {
        background: Background::Color(palette.bg_inset),
        border: Border::default()
            .width(if matches!(status, TextInputStatus::Focused { .. }) {
                Consts::BORDER_WIDTH
            } else {
                0.0
            })
            .color(palette.accent),
        icon: palette.muted,
        placeholder: palette.muted,
        value: palette.text,
        selection: palette.accent_soft,
    }
}

pub(crate) const fn border_width() -> f32 {
    Consts::BORDER_WIDTH
}

fn module_style(palette: RenderPalette) -> ContainerStyle {
    ContainerStyle::default()
        .background(Background::Color(palette.bg_inset))
        .border(
            Border::default()
                .width(Consts::BORDER_WIDTH)
                .color(palette.line),
        )
}

struct CornerTicks {
    color: iced::Color,
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
        let right = (bounds.width - Consts::CORNER_TICK_WIDTH).max(0.0);
        let bottom = (bounds.height - Consts::CORNER_TICK_WIDTH).max(0.0);
        let right_tick = (bounds.width - Consts::CORNER_TICK_SIZE).max(0.0);
        let bottom_tick = (bounds.height - Consts::CORNER_TICK_SIZE).max(0.0);

        frame.fill_rectangle(
            Point::ORIGIN,
            Size::new(Consts::CORNER_TICK_SIZE, Consts::CORNER_TICK_WIDTH),
            self.color,
        );
        frame.fill_rectangle(
            Point::ORIGIN,
            Size::new(Consts::CORNER_TICK_WIDTH, Consts::CORNER_TICK_SIZE),
            self.color,
        );
        frame.fill_rectangle(
            Point::new(right_tick, bottom),
            Size::new(Consts::CORNER_TICK_SIZE, Consts::CORNER_TICK_WIDTH),
            self.color,
        );
        frame.fill_rectangle(
            Point::new(right, bottom_tick),
            Size::new(Consts::CORNER_TICK_WIDTH, Consts::CORNER_TICK_SIZE),
            self.color,
        );

        vec![frame.into_geometry()]
    }
}
