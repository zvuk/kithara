use iced::{
    Alignment, Background, Border, Element, Length, Theme,
    widget::{
        Column, Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        row, text,
    },
    window,
};
use kithara_ui::{compile::CompiledNode, layout::Axis};
use num_traits::cast::AsPrimitive;

use super::{ModularMsg, controls, filter};
use crate::{
    gui::{app::Kithara, fonts, icons::Icon, message::Message, tokens::gap},
    theme::gui::GuiPalette,
};

pub(crate) fn render(state: &Kithara, _window: window::Id) -> Element<'_, Message> {
    let p = state.palette;
    let body = state.modular.compiled.as_ref().map_or_else(
        || empty_state(state),
        |compiled| {
            filter::visible(&compiled.root, &state.modular.hidden).map_or_else(
                || {
                    text("All modules are hidden")
                        .font(fonts::SANS)
                        .size(13.0)
                        .color(p.muted)
                        .into()
                },
                |root| render_compiled(state, &root),
            )
        },
    );

    let content = column![header(state), body]
        .spacing(gap::INLINE)
        .width(Length::Fill)
        .height(Length::Fill);
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(8)
        .style(move |_| ContainerStyle::default().background(Background::Color(p.bg)))
        .into()
}

fn header(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    row![
        text(state.modular.preset.clone())
            .font(fonts::MONO)
            .size(11.0)
            .color(p.text_dim),
        Space::new().width(Length::Fill),
        button(
            row![
                Icon::Settings.view(13.0, p.text),
                text("View settings").font(fonts::SANS).size(11.0),
            ]
            .spacing(gap::INLINE_TIGHT)
            .align_y(Alignment::Center),
        )
        .padding([4, 7])
        .style(move |theme, status| header_button_style(p, theme, status))
        .on_press(Message::Modular(ModularMsg::OpenSettings)),
        button(text("Exit").font(fonts::SANS).size(11.0))
            .padding([4, 7])
            .style(move |theme, status| header_button_style(p, theme, status))
            .on_press(Message::Modular(ModularMsg::Exit)),
    ]
    .spacing(gap::INLINE)
    .align_y(Alignment::Center)
    .width(Length::Fill)
    .into()
}

fn empty_state(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    if let Some(error) = &state.modular.error {
        return text(error.clone())
            .font(fonts::MONO)
            .size(11.0)
            .color(p.danger)
            .into();
    }
    button(text("Load preset").font(fonts::SANS).size(12.0))
        .padding([6, 10])
        .style(move |theme, status| header_button_style(p, theme, status))
        .on_press(Message::Modular(ModularMsg::Enter))
        .into()
}

fn render_compiled<'a>(state: &'a Kithara, node: &CompiledNode) -> Element<'a, Message> {
    match node {
        CompiledNode::Split { axis, children, .. } => match axis {
            Axis::Horizontal => Row::with_children(children.iter().map(|(weight, child)| {
                container(render_compiled(state, child))
                    .width(Length::FillPortion(fill_portion(*weight)))
                    .height(Length::Fill)
                    .into()
            }))
            .spacing(gap::INLINE)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
            Axis::Vertical => Column::with_children(children.iter().map(|(weight, child)| {
                container(render_compiled(state, child))
                    .width(Length::Fill)
                    .height(Length::FillPortion(fill_portion(*weight)))
                    .into()
            }))
            .spacing(gap::INLINE)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
            _ => Space::new().into(),
        },
        CompiledNode::Module { root, .. } => {
            let p = state.palette;
            container(controls::render_node(state, root))
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(8)
                .style(move |_| module_style(p))
                .into()
        }
        _ => Space::new().into(),
    }
}

fn fill_portion(weight: f32) -> u16 {
    let scaled = (weight * 100.0).round().max(1.0).min(f32::from(u16::MAX));
    scaled.as_()
}

fn module_style(p: GuiPalette) -> ContainerStyle {
    ContainerStyle::default()
        .background(Background::Color(p.bg_panel))
        .border(Border::default().width(1).color(p.line))
}

fn header_button_style(p: GuiPalette, _theme: &Theme, status: ButtonStatus) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered => p.bg_panel_2,
        ButtonStatus::Pressed => p.accent_soft,
        ButtonStatus::Active | ButtonStatus::Disabled => p.bg_inset,
    };
    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: p.text,
        border: Border::default().width(1).color(p.line),
        ..ButtonStyle::default()
    }
}
