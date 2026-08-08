use iced::{
    Alignment, Background, Border, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme,
    alignment::Vertical,
    widget::{
        Canvas, Row, Space, Stack,
        canvas::{self, Frame, Geometry},
        column, container,
        container::Style as ContainerStyle,
    },
};

use crate::{
    layout::FrameSides,
    module::ChromeStyle,
    render::{
        ChromeLeaf, IcedSkin, InputOwner, Skin, UiEvent, chrome_leaf, fonts, header_chevron,
        shaped_text,
    },
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct ModuleChrome<'a, Content> {
    skin: &'a Skin,
    #[builder(default)]
    style: ChromeStyle,
    content: Content,
    #[builder(default)]
    frame: FrameSides,
    chip: Option<&'a str>,
    drop: Option<DropZone>,
    footer: Option<String>,
    module: &'a str,
    input_owner: InputOwner,
    title: Option<&'a str>,
    assign: Vec<&'a str>,
    #[builder(default)]
    collapsed: bool,
    #[builder(default)]
    corners: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct DropZone {
    pub(crate) active: bool,
}

impl DropZone {
    pub(crate) const fn new(active: bool) -> Self {
        Self { active }
    }
}

impl<'a, Content> ModuleChrome<'a, Content>
where
    Content: Into<Element<'a, UiEvent>>,
{
    pub(crate) fn view(self) -> Element<'a, UiEvent> {
        module_view(self)
    }
}

impl<'a, Content> Widget<'a> for ModuleChrome<'a, Content>
where
    Content: Into<Element<'a, UiEvent>>,
{
    fn view(self) -> Element<'a, UiEvent> {
        module_view(self)
    }
}

fn module_view<'a, Content>(mut chrome: ModuleChrome<'a, Content>) -> Element<'a, UiEvent>
where
    Content: Into<Element<'a, UiEvent>>,
{
    let drop = chrome.drop.take();
    let accent = chrome.skin.palette.accent.into();
    let border_width = chrome.skin.chrome.frame.border_width;
    let shell = match chrome.style {
        ChromeStyle::Full => full(chrome),
        ChromeStyle::Frame => framed(
            chrome.content.into(),
            chrome.skin,
            chrome.skin.palette.bg_panel.into(),
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

/// Outlines the module while the enclosing host observes pointer crossings.
/// The host publishes without capturing, so controls inside keep every event
/// they would have had.
fn drop_zone<'a>(
    content: Element<'a, UiEvent>,
    zone: DropZone,
    accent: Color,
    border_width: f32,
) -> Element<'a, UiEvent> {
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
    outlined.into()
}

fn full<'a, Content>(chrome: ModuleChrome<'a, Content>) -> Element<'a, UiEvent>
where
    Content: Into<Element<'a, UiEvent>>,
{
    let skin = chrome.skin;
    let metrics = skin.chrome;
    let header = header(
        chrome.title,
        chrome.chip,
        chrome.assign,
        chrome.module,
        chrome.collapsed,
        skin,
        chrome.input_owner,
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

fn header<'a>(
    title: Option<&'a str>,
    chip: Option<&'a str>,
    assign: Vec<&'a str>,
    module: &str,
    collapsed: bool,
    skin: &'a Skin,
    input_owner: InputOwner,
) -> Element<'a, UiEvent> {
    let metrics = skin.chrome;
    let mut children = Vec::with_capacity(5 + assign.len());
    if let Some(chip) = chip {
        children.push(chrome_leaf(ChromeLeaf::Chip(chip), skin));
    }
    if let Some(title) = title {
        children.push(chrome_leaf(ChromeLeaf::Title(title), skin));
        children.push(vertical_line(skin));
    }
    children.push(Space::new().width(Length::Fill).into());
    children.extend(
        assign
            .into_iter()
            .map(|label| chrome_leaf(ChromeLeaf::Chip(label), skin)),
    );
    let chevron_background = skin.color(metrics.header_background);
    let chevron_border = skin.border(metrics.chevron_frame);
    children.push(
        container(Space::new())
            .width(Length::Fixed(metrics.chevron_size))
            .height(Length::Fill)
            .style(move |_| panel_frame_style(chevron_background, chevron_border))
            .into(),
    );
    let content = Row::with_children(children)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .height(Length::Fill);
    let content = Stack::with_children([
        content.into(),
        header_chevron(module, collapsed, skin, input_owner),
    ])
    .width(Length::Fill)
    .height(Length::Fill);

    let background = skin.color(metrics.header_background);
    let border = skin.border(metrics.header_frame);
    container(content)
        .width(Length::Fill)
        .height(Length::Fixed(metrics.header_height))
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

fn horizontal_line(skin: &Skin) -> Element<'_, UiEvent> {
    chrome_leaf(ChromeLeaf::HorizontalLine, skin)
}

fn vertical_line(skin: &Skin) -> Element<'_, UiEvent> {
    chrome_leaf(ChromeLeaf::VerticalLine, skin)
}

fn panel_frame_style(background: Color, border: Border) -> ContainerStyle {
    ContainerStyle::default()
        .background(Background::Color(background))
        .border(border)
}

fn panel_style(background: Color) -> ContainerStyle {
    ContainerStyle::default().background(Background::Color(background))
}

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
    frame_color: Color,
    sides: FrameSides,
    corners: Option<CornerTicks>,
    frame_width: f32,
}

#[derive(Clone, Copy)]
struct CornerTicks {
    color: Color,
    offset: f32,
    size: f32,
    width: f32,
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
