use iced::{
    Background, Border, Element, Length, Point, Rectangle, Renderer, Size, Theme,
    widget::{
        Canvas, Column, Row, Space, Stack, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        canvas::{self, Frame, Geometry},
        container,
        container::Style as ContainerStyle,
    },
};
use kithara_ui::{
    compile::{CompiledNode, CompiledUi},
    layout::Axis,
    registry::ControlCatalog,
    size::{Dim, SizeSpec},
};
use num_traits::cast::AsPrimitive;

use super::{ModularMsg, controls, endpoints, filter};
use crate::{
    gui::{
        app::Kithara,
        fonts,
        message::Message,
        tokens::{canvas as canvas_tokens, chrome, gap, type_scale},
        typography::shaped_text,
    },
    theme::gui::GuiPalette,
};

pub(crate) fn render(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let catalog = endpoints::render_catalog();
    let body = state.modular.compiled.as_ref().map_or_else(
        || empty_state(state),
        |compiled| {
            filter::visible(&compiled.root, &state.modular.hidden, compiled).map_or_else(
                || {
                    shaped_text("All modules are hidden")
                        .font(fonts::SANS)
                        .size(type_scale::BODY)
                        .color(p.canvas.muted)
                        .into()
                },
                |root| render_compiled(state, &root, compiled, catalog),
            )
        },
    );

    container(module_chrome(body, p))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(p.canvas.bg)))
        .into()
}

fn empty_state(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    if let Some(error) = &state.modular.error {
        return shaped_text(error.clone())
            .font(fonts::SANS)
            .size(canvas_tokens::ERROR_TEXT_SIZE)
            .color(p.danger)
            .into();
    }
    button(
        shaped_text("Load preset")
            .font(fonts::SANS)
            .size(canvas_tokens::LOAD_BUTTON_TEXT_SIZE),
    )
    .padding([
        canvas_tokens::LOAD_BUTTON_PADDING_Y,
        canvas_tokens::LOAD_BUTTON_PADDING_X,
    ])
    .style(move |theme, status| header_button_style(p, theme, status))
    .on_press(Message::Modular(ModularMsg::SelectPreset(
        state.modular.preset.clone(),
    )))
    .into()
}

fn render_compiled<'a>(
    state: &'a Kithara,
    node: &CompiledNode,
    ui: &'a CompiledUi,
    catalog: &dyn ControlCatalog,
) -> Element<'a, Message> {
    let p = state.palette;
    match node {
        CompiledNode::Split { axis, children, .. } => match axis {
            Axis::Horizontal => container(
                Row::with_children(children.iter().map(|(weight, child)| {
                    container(render_compiled(state, child, ui, catalog))
                        .width(split_length(child_size(child).w, *weight))
                        .height(Length::Fill)
                        .into()
                }))
                .spacing(gap::GRID)
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| {
                ContainerStyle::default().background(Background::Color(p.canvas.line_soft))
            })
            .into(),
            Axis::Vertical => container(
                Column::with_children(children.iter().map(|(weight, child)| {
                    container(render_compiled(state, child, ui, catalog))
                        .width(Length::Fill)
                        .height(split_length(child_size(child).h, *weight))
                        .into()
                }))
                .spacing(gap::GRID)
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| {
                ContainerStyle::default().background(Background::Color(p.canvas.line_soft))
            })
            .into(),
            _ => Space::new().into(),
        },
        CompiledNode::Module { root, .. } => controls::render_node(state, root, ui, catalog),
        _ => Space::new().into(),
    }
}

fn child_size(node: &CompiledNode) -> SizeSpec {
    match node {
        CompiledNode::Split { size, .. } | CompiledNode::Module { size, .. } => *size,
        _ => SizeSpec::FILL,
    }
}

fn split_length(dim: Dim, weight: f32) -> Length {
    match dim {
        Dim::Fixed(value) => Length::Fixed(value),
        _ => Length::FillPortion(fill_portion(weight)),
    }
}

pub(super) fn module_chrome<'a>(
    content: impl Into<Element<'a, Message>>,
    p: GuiPalette,
) -> Element<'a, Message> {
    let body = container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| module_style(p));
    let ticks = Canvas::new(CornerTicks { color: p.accent })
        .width(Length::Fill)
        .height(Length::Fill);

    Stack::with_children([body.into(), ticks.into()])
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn fill_portion(weight: f32) -> u16 {
    let scaled = (weight * canvas_tokens::FILL_WEIGHT_SCALE)
        .round()
        .max(1.0)
        .min(f32::from(u16::MAX));
    scaled.as_()
}

fn module_style(p: GuiPalette) -> ContainerStyle {
    ContainerStyle::default()
        .background(Background::Color(p.canvas.bg_inset))
        .border(
            Border::default()
                .width(chrome::BORDER_WIDTH)
                .color(p.canvas.line),
        )
}

fn header_button_style(p: GuiPalette, _theme: &Theme, status: ButtonStatus) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered => p.canvas.bg_panel_2,
        ButtonStatus::Pressed => p.accent_soft,
        ButtonStatus::Active | ButtonStatus::Disabled => p.canvas.bg_panel,
    };
    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: p.canvas.text,
        border: Border::default()
            .width(chrome::BORDER_WIDTH)
            .color(p.canvas.line),
        ..ButtonStyle::default()
    }
}

struct CornerTicks {
    color: iced::Color,
}

impl canvas::Program<Message> for CornerTicks {
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
        let tick = chrome::CORNER_TICK_SIZE;
        let width = chrome::CORNER_TICK_WIDTH;
        let right = (bounds.width - width).max(0.0);
        let bottom = (bounds.height - width).max(0.0);
        let right_tick = (bounds.width - tick).max(0.0);
        let bottom_tick = (bounds.height - tick).max(0.0);

        frame.fill_rectangle(Point::ORIGIN, Size::new(tick, width), self.color);
        frame.fill_rectangle(Point::ORIGIN, Size::new(width, tick), self.color);
        if bounds.height >= chrome::SHORT_MODULE_HEIGHT {
            frame.fill_rectangle(
                Point::new(right_tick, bottom),
                Size::new(tick, width),
                self.color,
            );
            frame.fill_rectangle(
                Point::new(right, bottom_tick),
                Size::new(width, tick),
                self.color,
            );
        }

        vec![frame.into_geometry()]
    }
}
