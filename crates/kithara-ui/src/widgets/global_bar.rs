use iced::{
    Alignment, Background, Border, Element, Length, Theme,
    alignment::Vertical,
    font::Weight,
    widget::{
        Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        container,
        container::Style as ContainerStyle,
        row,
    },
};

use super::chrome;
use crate::{
    builtin,
    render::{Icon, ReadValue, Reads, RenderPalette, UiEvent, fonts, shaped_text},
};

const BRAND_LETTERS: [&str; 7] = ["K", "I", "T", "H", "A", "R", "A"];

struct Consts;

impl Consts {
    const BRAND_GAP: f32 = 3.0;
    const BRAND_PADDING_X: f32 = 13.0;
    const BRAND_SIZE: f32 = 14.0;
    const CHIP_GAP: f32 = 1.0;
    const CHIP_PADDING_X: f32 = 8.0;
    const CHIP_PADDING_Y: f32 = 3.0;
    const CHIP_TEXT: f32 = 9.0;
    const GEAR_SIZE: f32 = 12.0;
    const HEIGHT: f32 = 34.0;
    const SELECTOR_PADDING_X: f32 = 10.0;
    const BRAND_WIDTH: f32 = 112.0;
    const SELECTOR_WIDTH: f32 = 126.0;
}

pub(crate) fn brand(palette: RenderPalette) -> Element<'static, UiEvent> {
    let letters = BRAND_LETTERS.into_iter().map(|letter| {
        shaped_text(letter)
            .font(fonts::display(Weight::Bold))
            .size(Consts::BRAND_SIZE)
            .color(palette.text)
            .into()
    });
    container(
        Row::with_children(letters)
            .spacing(Consts::BRAND_GAP)
            .align_y(Alignment::Center),
    )
    .padding([0.0, Consts::BRAND_PADDING_X])
    .width(Length::Fixed(Consts::BRAND_WIDTH))
    .height(Length::Fixed(Consts::HEIGHT))
    .align_y(Vertical::Center)
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
    .into()
}

pub(crate) fn preset_selector(
    reads: &dyn Reads,
    palette: RenderPalette,
) -> Element<'static, UiEvent> {
    let active = match reads.get("ui.preset") {
        Some(ReadValue::Text(preset)) => preset,
        _ => "",
    };
    let chips = row![
        preset_chip(
            palette,
            "MICRO",
            builtin::MICRO_PRESET,
            active == builtin::MICRO_PRESET,
        ),
        preset_chip(
            palette,
            "PLAYER",
            builtin::PLAYER_PRESET,
            active == builtin::PLAYER_PRESET,
        ),
    ]
    .spacing(Consts::CHIP_GAP)
    .align_y(Alignment::Center);

    container(container(chips).style(move |_| {
        ContainerStyle::default()
            .background(Background::Color(palette.line))
            .border(
                Border::default()
                    .width(chrome::border_width())
                    .color(palette.line),
            )
    }))
    .padding([0.0, Consts::SELECTOR_PADDING_X])
    .width(Length::Fixed(Consts::SELECTOR_WIDTH))
    .height(Length::Fixed(Consts::HEIGHT))
    .center_y(Length::Fill)
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
    .into()
}

pub(crate) fn spacer(palette: RenderPalette) -> Element<'static, UiEvent> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(Consts::HEIGHT))
        .align_y(Vertical::Center)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
        .into()
}

pub(crate) fn settings_button(palette: RenderPalette) -> Element<'static, UiEvent> {
    let icon = container(Icon::Gear.view(Consts::GEAR_SIZE, palette.text_dim))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill);
    button(icon)
        .padding(0.0)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(settings_button_style(palette))
        .on_press(UiEvent::OpenSettings)
        .into()
}

fn preset_chip(
    palette: RenderPalette,
    label: &'static str,
    preset: &'static str,
    active: bool,
) -> Element<'static, UiEvent> {
    button(
        container(
            shaped_text(label)
                .font(fonts::MONO)
                .size(Consts::CHIP_TEXT)
                .color(if active { palette.bg } else { palette.text_dim }),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill),
    )
    .padding([Consts::CHIP_PADDING_Y, Consts::CHIP_PADDING_X])
    .style(move |theme, status| preset_chip_style(palette, active, theme, status))
    .on_press(UiEvent::SelectPreset(preset.to_owned()))
    .into()
}

fn preset_chip_style(
    palette: RenderPalette,
    active: bool,
    _theme: &Theme,
    status: ButtonStatus,
) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered if active => palette.accent_strong,
        ButtonStatus::Hovered => palette.bg_panel_2,
        ButtonStatus::Pressed => palette.accent_soft,
        ButtonStatus::Active | ButtonStatus::Disabled if active => palette.accent,
        ButtonStatus::Active | ButtonStatus::Disabled => palette.bg_panel,
    };
    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: if active { palette.bg } else { palette.text_dim },
        ..ButtonStyle::default()
    }
}

fn settings_button_style(palette: RenderPalette) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle {
    move |_theme, status| {
        let background = match status {
            ButtonStatus::Hovered => palette.bg_panel_2,
            ButtonStatus::Pressed => palette.accent_soft,
            ButtonStatus::Active | ButtonStatus::Disabled => palette.bg_panel,
        };
        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color: palette.text_dim,
            ..ButtonStyle::default()
        }
    }
}
