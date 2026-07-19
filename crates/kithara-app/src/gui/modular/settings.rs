use iced::{
    Alignment, Background, Border, Element, Length, Size, Theme,
    font::Weight,
    widget::{
        Column, Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        scrollable,
    },
    window::Settings,
};
use kithara_ui::{
    builtin,
    compile::{CompiledNode, CompiledUi},
    render::{RenderPalette, UiEvent, fonts, shaped_text},
    widgets::{layout_preview, module_chrome, secondary_button_style},
};

use super::state::{BuiltinPreset, SettingsDraft, SettingsSection};
use crate::gui::{app::Kithara, message::Message};

struct Consts;

impl Consts {
    const ACTION_GAP: f32 = 8.0;
    const ACTION_PADDING_X: f32 = 16.0;
    const ACTION_PADDING_Y: f32 = 7.0;
    const BODY_TEXT_SIZE: f32 = 13.0;
    const CARD_GAP: f32 = 12.0;
    const CARD_PADDING: f32 = 8.0;
    const CARD_TITLE_SIZE: f32 = 14.0;
    const CONTENT_GAP: f32 = 14.0;
    const CONTENT_PADDING: f32 = 18.0;
    const FOOTER_PADDING_X: f32 = 16.0;
    const FOOTER_PADDING_Y: f32 = 12.0;
    const HEADER_PADDING_X: f32 = 16.0;
    const HEADER_PADDING_Y: f32 = 13.0;
    const INLINE_GAP: f32 = 8.0;
    const MICRO_LABEL_SIZE: f32 = 11.0;
    const MIN_WINDOW_HEIGHT: f32 = 400.0;
    const MIN_WINDOW_WIDTH: f32 = 560.0;
    const NAV_ITEM_HEIGHT: f32 = 42.0;
    const NAV_MARKER_WIDTH: f32 = 2.0;
    const NAV_PADDING_Y: f32 = 10.0;
    const NAV_TEXT_PADDING_X: f32 = 14.0;
    const NAV_WIDTH: f32 = 138.0;
    const OUTER_PADDING: f32 = 12.0;
    const ROW_GAP: f32 = 1.0;
    const ROW_PADDING_X: f32 = 10.0;
    const ROW_PADDING_Y: f32 = 8.0;
    const SEPARATOR_WIDTH: f32 = 1.0;
    const TITLE_SIZE: f32 = 16.0;
    const TOGGLE_PADDING_X: f32 = 8.0;
    const TOGGLE_PADDING_Y: f32 = 4.0;
    const WINDOW_HEIGHT: f32 = 460.0;
    const WINDOW_WIDTH: f32 = 640.0;
}

pub(super) fn window_settings() -> Settings {
    Settings {
        size: Size::new(Consts::WINDOW_WIDTH, Consts::WINDOW_HEIGHT),
        min_size: Some(Size::new(
            Consts::MIN_WINDOW_WIDTH,
            Consts::MIN_WINDOW_HEIGHT,
        )),
        exit_on_close_request: false,
        ..Settings::default()
    }
}

pub(crate) fn render(state: &Kithara) -> Element<'_, Message> {
    let palette = state.palette;
    let Some(draft) = state.modular.settings.as_ref() else {
        return container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg)))
            .into();
    };
    let header = container(
        shaped_text("View settings")
            .font(iced::Font {
                weight: Weight::Semibold,
                ..fonts::SANS
            })
            .size(Consts::TITLE_SIZE)
            .color(palette.text),
    )
    .padding([Consts::HEADER_PADDING_Y, Consts::HEADER_PADDING_X])
    .width(Length::Fill)
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)));
    let body = Row::with_children([
        navigation(palette, draft.section),
        vertical_separator(palette),
        section_content(palette, draft),
    ])
    .width(Length::Fill)
    .height(Length::Fill);
    let panel = column![
        header,
        horizontal_separator(palette),
        body,
        horizontal_separator(palette),
        footer(palette),
    ]
    .width(Length::Fill)
    .height(Length::Fill);
    let framed = module_chrome(panel, palette);

    container(framed)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(Consts::OUTER_PADDING)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg)))
        .into()
}

fn navigation(palette: RenderPalette, active: SettingsSection) -> Element<'static, Message> {
    container(
        column![
            nav_item(
                palette,
                "LAYOUT",
                active == SettingsSection::Layout,
                UiEvent::SettingsShowLayout,
            ),
            nav_item(
                palette,
                "MODULES",
                active == SettingsSection::Modules,
                UiEvent::SettingsShowModules,
            ),
        ]
        .spacing(Consts::ROW_GAP),
    )
    .padding([Consts::NAV_PADDING_Y, 0.0])
    .width(Length::Fixed(Consts::NAV_WIDTH))
    .height(Length::Fill)
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
    .into()
}

fn nav_item(
    palette: RenderPalette,
    label: &'static str,
    active: bool,
    event: UiEvent,
) -> Element<'static, Message> {
    let marker = container(Space::new())
        .width(Length::Fixed(Consts::NAV_MARKER_WIDTH))
        .height(Length::Fill)
        .style(move |_| {
            ContainerStyle::default().background(Background::Color(if active {
                palette.accent
            } else {
                iced::Color::TRANSPARENT
            }))
        });
    let label = container(
        shaped_text(label)
            .font(fonts::MONO)
            .size(Consts::MICRO_LABEL_SIZE)
            .color(if active {
                palette.accent
            } else {
                palette.text_dim
            }),
    )
    .padding([0.0, Consts::NAV_TEXT_PADDING_X])
    .width(Length::Fill)
    .align_y(iced::alignment::Vertical::Center);

    button(Row::with_children([marker.into(), label.into()]))
        .padding(0.0)
        .width(Length::Fill)
        .height(Length::Fixed(Consts::NAV_ITEM_HEIGHT))
        .style(move |theme, status| nav_style(palette, active, theme, status))
        .on_press(Message::Modular(event))
        .into()
}

fn section_content(palette: RenderPalette, draft: &SettingsDraft) -> Element<'static, Message> {
    let content = match draft.section {
        SettingsSection::Layout => layout_section(palette, draft),
        SettingsSection::Modules => modules_section(palette, draft),
    };
    container(content)
        .padding(Consts::CONTENT_PADDING)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_inset)))
        .into()
}

fn layout_section(palette: RenderPalette, draft: &SettingsDraft) -> Element<'static, Message> {
    let cards = Row::with_children([
        preset_card(
            palette,
            "Micro",
            builtin::MICRO_PRESET,
            draft.micro(),
            draft.draft_preset == BuiltinPreset::Micro,
        ),
        preset_card(
            palette,
            "Player",
            builtin::PLAYER_PRESET,
            draft.player(),
            draft.draft_preset == BuiltinPreset::Player,
        ),
    ])
    .spacing(Consts::CARD_GAP)
    .width(Length::Fill);

    column![section_title(palette, "LAYOUT"), cards]
        .spacing(Consts::CONTENT_GAP)
        .width(Length::Fill)
        .into()
}

fn preset_card(
    palette: RenderPalette,
    label: &'static str,
    preset: &'static str,
    compiled: &CompiledUi,
    selected: bool,
) -> Element<'static, Message> {
    button(
        column![
            layout_preview(compiled, palette),
            shaped_text(label)
                .font(iced::Font {
                    weight: Weight::Semibold,
                    ..fonts::SANS
                })
                .size(Consts::CARD_TITLE_SIZE)
                .color(if selected {
                    palette.accent
                } else {
                    palette.text
                }),
        ]
        .spacing(Consts::INLINE_GAP),
    )
    .padding(Consts::CARD_PADDING)
    .width(Length::FillPortion(1))
    .style(move |theme, status| preset_style(palette, selected, theme, status))
    .on_press(Message::Modular(UiEvent::SettingsSelectPreset(
        preset.to_owned(),
    )))
    .into()
}

fn modules_section(palette: RenderPalette, draft: &SettingsDraft) -> Element<'static, Message> {
    let rows = module_instances(draft.compiled())
        .into_iter()
        .map(|instance| {
            let visible = !draft.draft_hidden.contains(&instance);
            module_row(palette, instance, visible)
        });
    let modules = scrollable(
        Column::with_children(rows)
            .spacing(Consts::ROW_GAP)
            .width(Length::Fill),
    )
    .height(Length::Fill);

    column![section_title(palette, "MODULES"), modules]
        .spacing(Consts::CONTENT_GAP)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn module_row(
    palette: RenderPalette,
    instance: String,
    visible: bool,
) -> Element<'static, Message> {
    let message_instance = instance.clone();
    container(
        Row::with_children([
            shaped_text(instance)
                .font(fonts::SANS)
                .size(Consts::BODY_TEXT_SIZE)
                .color(palette.text)
                .width(Length::Fill)
                .into(),
            button(Row::with_children([
                toggle_segment(palette, "ON", visible),
                toggle_segment(palette, "OFF", !visible),
            ]))
            .padding(0.0)
            .style(move |theme, status| toggle_style(palette, theme, status))
            .on_press(Message::Modular(UiEvent::SettingsToggleModule(
                message_instance.clone(),
            )))
            .into(),
        ])
        .align_y(Alignment::Center)
        .spacing(Consts::INLINE_GAP),
    )
    .padding([Consts::ROW_PADDING_Y, Consts::ROW_PADDING_X])
    .width(Length::Fill)
    .style(move |_| {
        ContainerStyle::default()
            .background(Background::Color(palette.bg_panel))
            .border(Border::default().width(1.0).color(palette.line_soft))
    })
    .into()
}

fn toggle_segment(
    palette: RenderPalette,
    label: &'static str,
    active: bool,
) -> Element<'static, Message> {
    container(
        shaped_text(label)
            .font(fonts::MONO)
            .size(Consts::MICRO_LABEL_SIZE)
            .color(if active { palette.bg } else { palette.text_dim }),
    )
    .padding([Consts::TOGGLE_PADDING_Y, Consts::TOGGLE_PADDING_X])
    .style(move |_| {
        ContainerStyle::default().background(Background::Color(if active {
            palette.accent
        } else {
            palette.bg_panel
        }))
    })
    .into()
}

fn footer(palette: RenderPalette) -> Element<'static, Message> {
    let reset = button(action_label("Reset to preset", palette.text))
        .padding([Consts::ACTION_PADDING_Y, Consts::ACTION_PADDING_X])
        .style(secondary_button_style(palette))
        .on_press(Message::Modular(UiEvent::SettingsReset));
    let cancel = button(action_label("Cancel", palette.text))
        .padding([Consts::ACTION_PADDING_Y, Consts::ACTION_PADDING_X])
        .style(secondary_button_style(palette))
        .on_press(Message::Modular(UiEvent::CloseSettings));
    let done = button(action_label("Done", palette.bg))
        .padding([Consts::ACTION_PADDING_Y, Consts::ACTION_PADDING_X])
        .style(move |theme, status| primary_style(palette, theme, status))
        .on_press(Message::Modular(UiEvent::SettingsDone));

    container(
        Row::with_children([
            reset.into(),
            Space::new().width(Length::Fill).into(),
            cancel.into(),
            done.into(),
        ])
        .spacing(Consts::ACTION_GAP)
        .align_y(Alignment::Center),
    )
    .padding([Consts::FOOTER_PADDING_Y, Consts::FOOTER_PADDING_X])
    .width(Length::Fill)
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
    .into()
}

fn action_label(label: &'static str, color: iced::Color) -> Element<'static, Message> {
    shaped_text(label)
        .font(fonts::SANS)
        .size(Consts::BODY_TEXT_SIZE)
        .color(color)
        .into()
}

fn section_title(palette: RenderPalette, label: &'static str) -> Element<'static, Message> {
    shaped_text(label)
        .font(fonts::MONO)
        .size(Consts::MICRO_LABEL_SIZE)
        .color(palette.muted)
        .into()
}

fn vertical_separator(palette: RenderPalette) -> Element<'static, Message> {
    container(Space::new())
        .width(Length::Fixed(Consts::SEPARATOR_WIDTH))
        .height(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.line)))
        .into()
}

fn horizontal_separator(palette: RenderPalette) -> Element<'static, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(Consts::SEPARATOR_WIDTH))
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.line)))
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

fn nav_style(
    palette: RenderPalette,
    active: bool,
    _theme: &Theme,
    status: ButtonStatus,
) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered => palette.bg_panel_2,
        ButtonStatus::Pressed => palette.accent_soft,
        ButtonStatus::Active | ButtonStatus::Disabled if active => palette.accent_soft,
        ButtonStatus::Active | ButtonStatus::Disabled => palette.bg_panel,
    };
    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: if active {
            palette.accent
        } else {
            palette.text_dim
        },
        border: Border::default(),
        ..ButtonStyle::default()
    }
}

fn preset_style(
    palette: RenderPalette,
    selected: bool,
    _theme: &Theme,
    status: ButtonStatus,
) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered => palette.bg_panel_2,
        ButtonStatus::Pressed => palette.accent_soft,
        ButtonStatus::Active | ButtonStatus::Disabled => palette.bg_panel,
    };
    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: palette.text,
        border: Border::default().width(1.0).color(if selected {
            palette.accent
        } else {
            palette.line
        }),
        ..ButtonStyle::default()
    }
}

fn toggle_style(palette: RenderPalette, _theme: &Theme, status: ButtonStatus) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered => palette.bg_panel_2,
        ButtonStatus::Pressed => palette.accent_soft,
        ButtonStatus::Active | ButtonStatus::Disabled => palette.bg_deep,
    };
    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: palette.text_dim,
        border: Border::default().width(1.0).color(palette.line),
        ..ButtonStyle::default()
    }
}

fn primary_style(palette: RenderPalette, _theme: &Theme, status: ButtonStatus) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered => palette.accent_strong,
        ButtonStatus::Pressed => palette.accent_soft,
        ButtonStatus::Active | ButtonStatus::Disabled => palette.accent,
    };
    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: palette.bg,
        border: Border::default().width(1.0).color(palette.accent),
        ..ButtonStyle::default()
    }
}
