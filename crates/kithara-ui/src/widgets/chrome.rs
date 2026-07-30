use iced::{
    Alignment, Background, Border, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme,
    alignment::Vertical,
    widget::{
        Canvas, Row, Space, Stack, button,
        button::Style as ButtonStyle,
        canvas::{self, Frame, Geometry, Path, Stroke},
        column, container,
        container::Style as ContainerStyle,
        mouse_area,
    },
};

use crate::{
    layout::FrameSides,
    module::ChromeStyle,
    render::{Skin, UiEvent, fonts, shaped_text},
    skin::FontWeight,
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct ModuleChrome<'a, 'skin, Content, Message> {
    content: Content,
    title: Option<&'a str>,
    chip: Option<&'a str>,
    assign: Vec<&'a str>,
    #[builder(default)]
    style: ChromeStyle,
    #[builder(default)]
    frame: FrameSides,
    #[builder(default)]
    corners: bool,
    footer: Option<String>,
    on_toggle: Option<Message>,
    drop: Option<DropZone<Message>>,
    #[builder(default)]
    collapsed: bool,
    skin: &'skin Skin,
}

pub(crate) struct DropZone<Message> {
    pub(crate) on_enter: Message,
    pub(crate) on_exit: Message,
    pub(crate) active: bool,
}

impl<Message> DropZone<Message> {
    pub(crate) const fn new(on_enter: Message, on_exit: Message, active: bool) -> Self {
        Self {
            on_enter,
            on_exit,
            active,
        }
    }
}

impl<'a, Content, Message> ModuleChrome<'a, '_, Content, Message>
where
    Content: Into<Element<'a, Message>>,
    Message: Clone + 'a,
{
    pub(crate) fn view(self) -> Element<'a, Message> {
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
    mut chrome: ModuleChrome<'a, '_, Content, Message>,
) -> Element<'a, Message>
where
    Content: Into<Element<'a, Message>>,
    Message: Clone + 'a,
{
    let drop = chrome.drop.take();
    let accent = chrome.skin.palette.accent;
    let border_width = chrome.skin.chrome.frame.border_width;
    let shell = match chrome.style {
        ChromeStyle::Full => full(chrome),
        ChromeStyle::Frame => framed(
            chrome.content.into(),
            chrome.skin,
            chrome.skin.palette.bg_panel,
            Length::Fill,
            chrome.frame,
            chrome.corners,
        ),
        ChromeStyle::Plain => chrome.content.into(),
    };
    match drop {
        Some(zone) => drop_zone(shell, zone, accent, border_width),
        None => shell,
    }
}

/// Reports the pointer crossing the module and outlines it while a drag is
/// over it. The area publishes without capturing, so the controls inside keep
/// every event they would have had.
fn drop_zone<'a, Message>(
    content: Element<'a, Message>,
    zone: DropZone<Message>,
    accent: Color,
    border_width: f32,
) -> Element<'a, Message>
where
    Message: Clone + 'a,
{
    let active = zone.active;
    let outlined = container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| {
            let border = if active {
                Border::default().color(accent).width(border_width)
            } else {
                Border::default()
            };
            ContainerStyle::default().border(border)
        });
    mouse_area(outlined)
        .on_enter(zone.on_enter)
        .on_exit(zone.on_exit)
        .into()
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
        chrome.assign,
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
            chrome.frame,
            chrome.corners,
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
        chrome.frame,
        chrome.corners,
    )
}

fn header<'a, Message>(
    title: Option<&'a str>,
    chip: Option<&'a str>,
    assign: Vec<&'a str>,
    on_toggle: Option<Message>,
    collapsed: bool,
    skin: &Skin,
) -> Element<'a, Message>
where
    Message: Clone + 'a,
{
    let metrics = skin.chrome;
    let mut children = Vec::with_capacity(5 + assign.len());
    if let Some(chip) = chip {
        children.push(header_chip(chip, skin));
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
    children.extend(assign.into_iter().map(|label| header_chip(label, skin)));
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

fn header_chip<'a, Message>(label: &'a str, skin: &Skin) -> Element<'a, Message>
where
    Message: 'a,
{
    let metrics = skin.chrome;
    let background = skin.color(metrics.chip_background);
    let text = skin.color(metrics.chip_text);
    let border = skin.border(metrics.chip_frame);
    container(
        shaped_text(label)
            .font(fonts::MONO)
            .size(metrics.chip_text_size)
            .color(text),
    )
    .padding([0.0, metrics.chip_pad])
    .height(Length::Fill)
    .align_y(Vertical::Center)
    .style(move |_| panel_frame_style(background, border))
    .into()
}

fn framed<'a, Message>(
    content: Element<'a, Message>,
    skin: &Skin,
    background: Color,
    height: Length,
    sides: FrameSides,
    corners: bool,
) -> Element<'a, Message>
where
    Message: 'a,
{
    let body = container(content)
        .width(Length::Fill)
        .height(height)
        .style(move |_| panel_style(background));
    let frame = Canvas::new(FrameChrome {
        sides,
        frame_color: skin.color(skin.chrome.frame.border),
        frame_width: skin.chrome.frame.border_width,
        corners: corners.then(|| CornerTicks::from(skin)),
    })
    .width(Length::Fill)
    .height(height);

    Stack::with_children([body.into(), frame.into()])
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

fn panel_frame_style(background: Color, border: Border) -> ContainerStyle {
    ContainerStyle::default()
        .background(Background::Color(background))
        .border(border)
}

fn panel_style(background: Color) -> ContainerStyle {
    ContainerStyle::default().background(Background::Color(background))
}

/// Border overlay for a container: hairlines on the requested sides, in the
/// colour and width the caller selected.
pub(crate) fn frame_overlay<'a, Message>(
    content: Element<'a, Message>,
    sides: FrameSides,
    size: (Length, Length),
    color: Color,
    line_width: f32,
) -> Element<'a, Message>
where
    Message: 'a,
{
    let (width, height) = size;
    let frame = Canvas::new(FrameChrome {
        sides,
        frame_color: color,
        frame_width: line_width,
        corners: None,
    })
    .width(Length::Fill)
    .height(Length::Fill);
    let body = container(content).width(width).height(height);
    Stack::with_children([body.into(), frame.into()])
        .width(width)
        .height(height)
        .into()
}

struct FrameChrome {
    sides: FrameSides,
    frame_color: Color,
    frame_width: f32,
    corners: Option<CornerTicks>,
}

#[derive(Clone, Copy)]
struct CornerTicks {
    color: Color,
    size: f32,
    width: f32,
    offset: f32,
}

impl From<&Skin> for CornerTicks {
    fn from(skin: &Skin) -> Self {
        Self {
            color: skin.color(skin.chrome.corner_color),
            size: skin.chrome.corner_size,
            width: skin.chrome.corner_width,
            offset: skin.chrome.corner_offset,
        }
    }
}

impl CornerTicks {
    fn marks(self, bounds: Rectangle) -> [(Point, Size); 4] {
        let along = Size::new(self.size, self.width);
        let across = Size::new(self.width, self.size);
        let near = Point::new(self.offset, self.offset);
        let far_x = (bounds.width - self.offset - self.width).max(0.0);
        let far_y = (bounds.height - self.offset - self.width).max(0.0);
        let tail_x = (bounds.width - self.offset - self.size).max(0.0);
        let tail_y = (bounds.height - self.offset - self.size).max(0.0);
        [
            (near, along),
            (near, across),
            (Point::new(tail_x, far_y), along),
            (Point::new(far_x, tail_y), across),
        ]
    }
}

impl<Message> canvas::Program<Message> for FrameChrome {
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
        let right = (bounds.width - self.frame_width).max(0.0);
        let bottom = (bounds.height - self.frame_width).max(0.0);
        if self.sides.top {
            frame.fill_rectangle(
                Point::ORIGIN,
                Size::new(bounds.width, self.frame_width),
                self.frame_color,
            );
        }
        if self.sides.right {
            frame.fill_rectangle(
                Point::new(right, 0.0),
                Size::new(self.frame_width, bounds.height),
                self.frame_color,
            );
        }
        if self.sides.bottom {
            frame.fill_rectangle(
                Point::new(0.0, bottom),
                Size::new(bounds.width, self.frame_width),
                self.frame_color,
            );
        }
        if self.sides.left {
            frame.fill_rectangle(
                Point::ORIGIN,
                Size::new(self.frame_width, bounds.height),
                self.frame_color,
            );
        }
        if let Some(ticks) = self.corners {
            for (origin, size) in ticks.marks(bounds) {
                frame.fill_rectangle(origin, size, ticks.color);
            }
        }
        vec![frame.into_geometry()]
    }
}
