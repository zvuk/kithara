use iced::{
    Alignment, Background, Border, Element, Length, Theme,
    font::Weight,
    widget::{
        Column, Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        scrollable, text,
    },
};
use kithara_ui::{
    builtin,
    compile::{CompiledNode, CompiledUi},
};

use super::ModularMsg;
use crate::{
    gui::{app::Kithara, fonts, message::Message, tokens::gap},
    theme::gui::GuiPalette,
};

pub(crate) fn render(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let layout = Row::with_children([
        preset_card(
            p,
            "Micro",
            builtin::MICRO_PRESET,
            state.modular.preset == builtin::MICRO_PRESET,
        ),
        preset_card(
            p,
            "Player",
            builtin::PLAYER_PRESET,
            state.modular.preset == builtin::PLAYER_PRESET,
        ),
    ])
    .spacing(gap::INLINE_WIDE)
    .width(Length::Fill);

    let modules = state
        .modular
        .compiled
        .as_ref()
        .map(module_instances)
        .unwrap_or_default();
    let module_rows = modules.into_iter().map(|instance| {
        let visible = !state.modular.hidden.contains(&instance);
        module_row(p, instance, visible)
    });
    let module_list: Element<'_, Message> = if state.modular.compiled.is_some() {
        scrollable(
            Column::with_children(module_rows)
                .spacing(gap::INLINE_TIGHT)
                .width(Length::Fill),
        )
        .height(Length::Fill)
        .into()
    } else {
        text("No preset loaded")
            .font(fonts::SANS)
            .size(13.0)
            .color(p.muted)
            .into()
    };

    let content = column![
        section_title(p, "Layout"),
        layout,
        section_title(p, "Modules"),
        module_list,
        Row::with_children([
            Space::new().width(Length::Fill).into(),
            button(text("Done").font(fonts::SANS).size(13.0))
                .padding([7, 16])
                .style(move |theme, status| action_style(p, theme, status))
                .on_press(Message::Modular(ModularMsg::CloseSettings))
                .into(),
        ])
        .align_y(Alignment::Center)
        .width(Length::Fill),
    ]
    .spacing(gap::CONTENT)
    .width(Length::Fill)
    .height(Length::Fill);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(16)
        .style(move |_| ContainerStyle::default().background(Background::Color(p.bg)))
        .into()
}

fn preset_card(
    p: GuiPalette,
    label: &'static str,
    preset: &'static str,
    selected: bool,
) -> Element<'static, Message> {
    button(
        column![
            text(label)
                .font(fonts::display(Weight::Semibold))
                .size(15.0),
            text(if selected { "Selected" } else { "Select" })
                .font(fonts::MONO)
                .size(10.0)
                .color(if selected { p.accent } else { p.muted }),
        ]
        .spacing(gap::INLINE_TIGHT),
    )
    .padding([10, 12])
    .width(Length::FillPortion(1))
    .style(move |theme, status| preset_style(p, selected, theme, status))
    .on_press(Message::Modular(ModularMsg::SelectPreset(
        preset.to_owned(),
    )))
    .into()
}

fn module_row(p: GuiPalette, instance: String, visible: bool) -> Element<'static, Message> {
    let message_instance = instance.clone();
    container(
        Row::with_children([
            text(instance)
                .font(fonts::SANS)
                .size(13.0)
                .color(p.text)
                .width(Length::Fill)
                .into(),
            button(
                text(if visible { "ON" } else { "OFF" })
                    .font(fonts::MONO)
                    .size(10.0),
            )
            .padding([4, 8])
            .style(move |theme, status| toggle_style(p, visible, theme, status))
            .on_press(Message::Modular(ModularMsg::ToggleModule(
                message_instance.clone(),
            )))
            .into(),
        ])
        .align_y(Alignment::Center)
        .spacing(gap::INLINE_WIDE),
    )
    .padding([6, 8])
    .width(Length::Fill)
    .style(move |_| {
        ContainerStyle::default()
            .background(Background::Color(p.bg_panel))
            .border(Border::default().width(1).color(p.line_soft))
    })
    .into()
}

fn section_title(p: GuiPalette, label: &'static str) -> Element<'static, Message> {
    text(label)
        .font(fonts::display(Weight::Semibold))
        .size(14.0)
        .color(p.text)
        .into()
}

fn module_instances(compiled: &CompiledUi) -> Vec<String> {
    let mut instances = Vec::new();
    collect_modules(&compiled.root, compiled, &mut instances);
    instances
}

fn collect_modules(node: &CompiledNode, ui: &CompiledUi, instances: &mut Vec<String>) {
    match node {
        CompiledNode::Split { children, .. } => {
            for (_, child) in children {
                collect_modules(child, ui, instances);
            }
        }
        CompiledNode::Module { instance, .. } => {
            instances.push(ui.resolve(*instance).to_owned());
        }
        _ => {}
    }
}

fn preset_style(
    p: GuiPalette,
    selected: bool,
    _theme: &Theme,
    status: ButtonStatus,
) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered => p.bg_panel_2,
        ButtonStatus::Pressed => p.accent_soft,
        ButtonStatus::Active | ButtonStatus::Disabled => p.bg_panel,
    };
    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: p.text,
        border: Border::default()
            .width(1)
            .color(if selected { p.accent } else { p.line }),
        ..ButtonStyle::default()
    }
}

fn toggle_style(p: GuiPalette, visible: bool, _theme: &Theme, status: ButtonStatus) -> ButtonStyle {
    let base = if visible { p.accent } else { p.bg_inset };
    let background = match status {
        ButtonStatus::Hovered => p.bg_panel_2,
        ButtonStatus::Pressed => p.accent_soft,
        ButtonStatus::Active | ButtonStatus::Disabled => base,
    };
    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: if visible { p.bg_deep } else { p.text_dim },
        border: Border::default().width(1).color(p.line),
        ..ButtonStyle::default()
    }
}

fn action_style(p: GuiPalette, _theme: &Theme, status: ButtonStatus) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered => p.accent_strong,
        ButtonStatus::Pressed => p.accent_soft,
        ButtonStatus::Active | ButtonStatus::Disabled => p.accent,
    };
    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: p.bg_deep,
        border: Border::default().width(1).color(p.accent),
        ..ButtonStyle::default()
    }
}
