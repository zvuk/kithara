use iced::{
    Alignment, Background, Border, Element, Length, Theme,
    font::Weight,
    widget::{
        Column, Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        scrollable,
    },
};
use kithara_ui::{
    builtin,
    compile::{CompiledNode, CompiledUi},
    render::{RenderPalette, UiEvent, fonts, shaped_text},
};

use super::view::module_chrome;
use crate::gui::{
    app::Kithara,
    message::Message,
    tokens::{gap, settings as settings_tokens, type_scale},
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
    .spacing(gap::GRID)
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
                .spacing(gap::GRID)
                .width(Length::Fill),
        )
        .height(Length::Fill)
        .into()
    } else {
        shaped_text("No preset loaded")
            .font(fonts::SANS)
            .size(type_scale::BODY)
            .color(p.muted)
            .into()
    };

    let content = column![
        section_title(p, "LAYOUT"),
        layout,
        section_title(p, "MODULES"),
        module_list,
        Row::with_children([
            Space::new().width(Length::Fill).into(),
            button(shaped_text("Done").font(fonts::SANS).size(type_scale::BODY),)
                .padding([
                    settings_tokens::ACTION_PADDING_Y,
                    settings_tokens::ACTION_PADDING_X,
                ])
                .style(move |theme, status| action_style(p, theme, status))
                .on_press(Message::Modular(UiEvent::CloseSettings))
                .into(),
        ])
        .align_y(Alignment::Center)
        .width(Length::Fill),
    ]
    .spacing(gap::CONTENT)
    .width(Length::Fill)
    .height(Length::Fill);
    let content = container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(settings_tokens::CONTENT_PADDING);
    let framed = module_chrome(content, p);

    container(framed)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(settings_tokens::OUTER_PADDING)
        .style(move |_| ContainerStyle::default().background(Background::Color(p.bg)))
        .into()
}

fn preset_card(
    p: RenderPalette,
    label: &'static str,
    preset: &'static str,
    selected: bool,
) -> Element<'static, Message> {
    button(
        column![
            shaped_text(label)
                .font(iced::Font {
                    weight: Weight::Semibold,
                    ..fonts::SANS
                })
                .size(settings_tokens::CARD_TITLE_SIZE)
                .color(p.text),
            shaped_text(if selected { "Selected" } else { "Select" })
                .font(fonts::SANS)
                .size(type_scale::BODY)
                .color(if selected { p.accent } else { p.muted }),
        ]
        .spacing(gap::INLINE_TIGHT),
    )
    .padding([
        settings_tokens::CARD_PADDING_Y,
        settings_tokens::CARD_PADDING_X,
    ])
    .width(Length::FillPortion(1))
    .style(move |theme, status| preset_style(p, selected, theme, status))
    .on_press(Message::Modular(UiEvent::SelectPreset(preset.to_owned())))
    .into()
}

fn module_row(p: RenderPalette, instance: String, visible: bool) -> Element<'static, Message> {
    let message_instance = instance.clone();
    container(
        Row::with_children([
            shaped_text(instance)
                .font(fonts::SANS)
                .size(type_scale::BODY)
                .color(p.text)
                .width(Length::Fill)
                .into(),
            button(Row::with_children([
                toggle_segment(p, "ON", visible),
                toggle_segment(p, "OFF", !visible),
            ]))
            .padding(0)
            .style(move |theme, status| toggle_style(p, theme, status))
            .on_press(Message::Modular(UiEvent::ToggleModule(
                message_instance.clone(),
            )))
            .into(),
        ])
        .align_y(Alignment::Center)
        .spacing(gap::INLINE_WIDE),
    )
    .padding([
        settings_tokens::ROW_PADDING_Y,
        settings_tokens::ROW_PADDING_X,
    ])
    .width(Length::Fill)
    .style(move |_| {
        ContainerStyle::default()
            .background(Background::Color(p.bg_panel))
            .border(Border::default().width(1).color(p.line_soft))
    })
    .into()
}

fn toggle_segment(
    p: RenderPalette,
    label: &'static str,
    active: bool,
) -> Element<'static, Message> {
    container(
        shaped_text(label)
            .font(fonts::SANS)
            .size(type_scale::MICRO_LABEL)
            .color(if active { p.bg } else { p.text_dim }),
    )
    .padding([
        settings_tokens::TOGGLE_PADDING_Y,
        settings_tokens::TOGGLE_PADDING_X,
    ])
    .style(move |_| {
        ContainerStyle::default().background(Background::Color(if active {
            p.accent
        } else {
            p.bg_panel
        }))
    })
    .into()
}

fn section_title(p: RenderPalette, label: &'static str) -> Element<'static, Message> {
    shaped_text(label)
        .font(fonts::SANS)
        .size(type_scale::MICRO_LABEL)
        .color(p.muted)
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
    p: RenderPalette,
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

fn toggle_style(p: RenderPalette, _theme: &Theme, status: ButtonStatus) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered => p.bg_panel_2,
        ButtonStatus::Pressed => p.accent_soft,
        ButtonStatus::Active | ButtonStatus::Disabled => p.bg_deep,
    };
    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: p.text_dim,
        border: Border::default().width(1).color(p.line),
        ..ButtonStyle::default()
    }
}

fn action_style(p: RenderPalette, _theme: &Theme, status: ButtonStatus) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered => p.accent_strong,
        ButtonStatus::Pressed => p.accent_soft,
        ButtonStatus::Active | ButtonStatus::Disabled => p.accent,
    };
    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: p.bg,
        border: Border::default().width(1).color(p.accent),
        ..ButtonStyle::default()
    }
}
