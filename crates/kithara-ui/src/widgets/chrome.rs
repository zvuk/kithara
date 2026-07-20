use iced::{
    Alignment, Background, Border, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme,
    alignment::Vertical,
    widget::{
        Canvas, Row, Space, Stack, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        canvas::{self, Frame, Geometry, Path, Stroke},
        column, container,
        container::Style as ContainerStyle,
    },
};

use crate::{
    module::ChromeStyle,
    render::{Skin, UiEvent, fonts, shaped_text},
    skin::FontWeight,
    widgets::Widget,
};

/// Framed module shell shared by renderer and application surfaces.
#[derive(bon::Builder)]
#[non_exhaustive]
pub struct ModuleChrome<'a, 'skin, Content, Message> {
    content: Content,
    title: Option<&'a str>,
    chip: Option<&'a str>,
    #[builder(default)]
    style: ChromeStyle,
    footer: Option<String>,
    on_toggle: Option<Message>,
    #[builder(default)]
    collapsed: bool,
    skin: &'skin Skin,
}

impl<'a, Content, Message> ModuleChrome<'a, '_, Content, Message>
where
    Content: Into<Element<'a, Message>>,
    Message: Clone + 'a,
{
    #[must_use]
    pub fn view(self) -> Element<'a, Message> {
        module_view(self)
    }
}

impl<'a, Content> Widget<'a> for ModuleChrome<'a, '_, Content, UiEvent>
where
    Content: Into<Element<'a, UiEvent>>,
{
    fn view(self) -> Element<'a, UiEvent> {
        module_view(self)
    }
}

fn module_view<'a, Content, Message>(
    chrome: ModuleChrome<'a, '_, Content, Message>,
) -> Element<'a, Message>
where
    Content: Into<Element<'a, Message>>,
    Message: Clone + 'a,
{
    match chrome.style {
        ChromeStyle::Full => full(chrome),
        ChromeStyle::Frame => framed(
            chrome.content.into(),
            chrome.skin,
            chrome.skin.palette.bg_panel,
            Length::Fill,
        ),
        ChromeStyle::Plain => chrome.content.into(),
    }
}

fn full<'a, Content, Message>(
    chrome: ModuleChrome<'a, '_, Content, Message>,
) -> Element<'a, Message>
where
    Content: Into<Element<'a, Message>>,
    Message: Clone + 'a,
{
    let skin = chrome.skin;
    let metrics = skin.chrome;
    let header = header(
        chrome.title,
        chrome.chip,
        chrome.on_toggle,
        chrome.collapsed,
        skin,
    );
    if chrome.collapsed {
        return framed(
            header,
            skin,
            skin.color(metrics.panel_background),
            Length::Fixed(metrics.header_height),
        );
    }

    let panel_background = skin.color(metrics.panel_background);
    let footer_background = skin.color(metrics.footer_background);
    let footer_text = skin.color(metrics.footer_text);
    let footer_border = skin.border(metrics.footer_frame);
    let content = container(chrome.content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| panel_style(panel_background));
    let footer = container(
        shaped_text(chrome.footer.unwrap_or_default())
            .font(fonts::MONO)
            .size(metrics.footer_text_size)
            .color(footer_text),
    )
    .padding([0.0, metrics.footer_pad])
    .width(Length::Fill)
    .height(Length::Fixed(metrics.footer_height))
    .align_y(Vertical::Center)
    .style(move |_| panel_frame_style(footer_background, footer_border));
    let shell = column![
        header,
        horizontal_line(skin),
        content,
        horizontal_line(skin),
        footer,
    ]
    .width(Length::Fill)
    .height(Length::Fill);

    framed(
        shell.into(),
        skin,
        skin.color(metrics.panel_background),
        Length::Fill,
    )
}

fn header<'a, Message>(
    title: Option<&'a str>,
    chip: Option<&'a str>,
    on_toggle: Option<Message>,
    collapsed: bool,
    skin: &Skin,
) -> Element<'a, Message>
where
    Message: Clone + 'a,
{
    let metrics = skin.chrome;
    let mut children = Vec::with_capacity(5);
    if let Some(chip) = chip {
        let background = skin.color(metrics.chip_background);
        let text = skin.color(metrics.chip_text);
        let border = skin.border(metrics.chip_frame);
        children.push(
            container(
                shaped_text(chip)
                    .font(fonts::MONO)
                    .size(metrics.chip_text_size)
                    .color(text),
            )
            .padding([0.0, metrics.chip_pad])
            .height(Length::Fill)
            .align_y(Vertical::Center)
            .style(move |_| panel_frame_style(background, border))
            .into(),
        );
    }
    if let Some(title) = title {
        let background = skin.color(metrics.title_background);
        let text = skin.color(metrics.title_text);
        let border = skin.border(metrics.title_frame);
        children.push(
            container(
                shaped_text(title)
                    .font(fonts::display(FontWeight::Medium))
                    .size(metrics.title_text_size)
                    .color(text),
            )
            .padding([0.0, metrics.chip_pad])
            .height(Length::Fill)
            .align_y(Vertical::Center)
            .style(move |_| panel_frame_style(background, border))
            .into(),
        );
        children.push(vertical_line(skin));
    }
    children.push(Space::new().width(Length::Fill).into());
    let chevron_background = skin.color(metrics.header_background);
    let chevron_border = skin.border(metrics.chevron_frame);
    children.push(
        container(
            Canvas::new(Chevron {
                collapsed,
                color: skin.color(metrics.chevron_color),
                line_color: skin.color(metrics.inner_line),
                icon_size: metrics.chevron_icon_size,
                stroke_width: metrics.chevron_stroke_width,
                line_width: metrics.inner_line_width,
            })
            .width(Length::Fixed(metrics.chevron_size))
            .height(Length::Fill),
        )
        .width(Length::Fixed(metrics.chevron_size))
        .height(Length::Fill)
        .style(move |_| panel_frame_style(chevron_background, chevron_border))
        .into(),
    );
    let content = Row::with_children(children)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .height(Length::Fill);

    let background = skin.color(metrics.header_background);
    let text = skin.color(metrics.title_text);
    let border = skin.border(metrics.header_frame);
    button(content)
        .padding(0.0)
        .width(Length::Fill)
        .height(Length::Fixed(metrics.header_height))
        .style(move |_theme, _status| ButtonStyle {
            background: Some(Background::Color(background)),
            text_color: text,
            border,
            ..ButtonStyle::default()
        })
        .on_press_maybe(on_toggle)
        .into()
}

fn framed<'a, Message>(
    content: Element<'a, Message>,
    skin: &Skin,
    background: Color,
    height: Length,
) -> Element<'a, Message>
where
    Message: 'a,
{
    let border = skin.border(skin.chrome.frame);
    let body = container(content)
        .width(Length::Fill)
        .height(height)
        .style(move |_| panel_frame_style(background, border));
    let ticks = Canvas::new(CornerTicks {
        color: skin.color(skin.chrome.corner_color),
        size: skin.chrome.corner_size,
        width: skin.chrome.corner_width,
        offset: skin.chrome.corner_offset,
    })
    .width(Length::Fill)
    .height(height);

    Stack::with_children([body.into(), ticks.into()])
        .width(Length::Fill)
        .height(height)
        .into()
}

fn horizontal_line<'a, Message>(skin: &Skin) -> Element<'a, Message>
where
    Message: 'a,
{
    let color = skin.color(skin.chrome.inner_line);
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(skin.chrome.inner_line_width))
        .style(move |_| panel_style(color))
        .into()
}

fn vertical_line<'a, Message>(skin: &Skin) -> Element<'a, Message>
where
    Message: 'a,
{
    let color = skin.color(skin.chrome.inner_line);
    container(Space::new())
        .width(Length::Fixed(skin.chrome.inner_line_width))
        .height(Length::Fill)
        .style(move |_| panel_style(color))
        .into()
}

struct Chevron {
    collapsed: bool,
    color: Color,
    line_color: Color,
    icon_size: f32,
    stroke_width: f32,
    line_width: f32,
}

impl<Message> canvas::Program<Message> for Chevron {
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
            Size::new(self.line_width, bounds.height),
            self.line_color,
        );
        let center = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let half = self.icon_size / 2.0;
        let rise = self.icon_size / 4.0;
        let direction = if self.collapsed { 1.0 } else { -1.0 };
        let path = Path::new(|builder| {
            builder.move_to(Point::new(center.x - half, center.y - rise * direction));
            builder.line_to(Point::new(center.x, center.y + rise * direction));
            builder.line_to(Point::new(center.x + half, center.y - rise * direction));
        });
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(self.color)
                .with_width(self.stroke_width),
        );
        vec![frame.into_geometry()]
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

fn panel_frame_style(background: Color, border: Border) -> ContainerStyle {
    ContainerStyle::default()
        .background(Background::Color(background))
        .border(border)
}

fn panel_style(background: Color) -> ContainerStyle {
    ContainerStyle::default().background(Background::Color(background))
}

struct CornerTicks {
    color: Color,
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
