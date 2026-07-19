use iced::{
    Alignment, Background, Element, Length, Theme,
    alignment::Vertical,
    widget::{
        Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        container,
        container::Style as ContainerStyle,
        row,
    },
};

use crate::{
    builtin,
    render::{Icon, ReadValue, Reads, Skin, UiEvent, fonts, shaped_text},
};

const BRAND_LETTERS: [&str; 7] = ["K", "I", "T", "H", "A", "R", "A"];

pub(crate) fn brand(skin: &Skin) -> Element<'static, UiEvent> {
    let palette = skin.palette;
    let letters = BRAND_LETTERS.into_iter().map(|letter| {
        shaped_text(letter)
            .font(fonts::display(skin.global_bar.brand_text.weight))
            .size(skin.global_bar.brand_text.size)
            .color(palette.text)
            .into()
    });
    container(
        Row::with_children(letters)
            .spacing(skin.global_bar.brand_gap)
            .align_y(Alignment::Center),
    )
    .padding([
        skin.global_bar.brand_padding_y,
        skin.global_bar.brand_padding_x,
    ])
    .width(Length::Fixed(skin.global_bar.brand_width))
    .height(Length::Fixed(skin.global_bar.height))
    .align_y(Vertical::Center)
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
    .into()
}

pub(crate) fn preset_selector(reads: &dyn Reads, skin: &Skin) -> Element<'static, UiEvent> {
    let palette = skin.palette;
    let active = match reads.get("ui.preset") {
        Some(ReadValue::Text(preset)) => preset,
        _ => "",
    };
    let chips = row![
        preset_chip(
            skin,
            "MICRO",
            builtin::MICRO_PRESET,
            active == builtin::MICRO_PRESET,
        ),
        preset_chip(
            skin,
            "PLAYER",
            builtin::PLAYER_PRESET,
            active == builtin::PLAYER_PRESET,
        ),
    ]
    .spacing(skin.global_bar.chip_gap)
    .align_y(Alignment::Center);

    let selector_border = skin.border(skin.global_bar.selector_frame);
    container(container(chips).style(move |_| {
        ContainerStyle::default()
            .background(Background::Color(palette.line))
            .border(selector_border)
    }))
    .padding([
        skin.global_bar.selector_padding_y,
        skin.global_bar.selector_padding_x,
    ])
    .width(Length::Fixed(skin.global_bar.selector_width))
    .height(Length::Fixed(skin.global_bar.height))
    .center_y(Length::Fill)
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
    .into()
}

pub(crate) fn spacer(skin: &Skin) -> Element<'static, UiEvent> {
    let palette = skin.palette;
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(skin.global_bar.height))
        .align_y(Vertical::Center)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
        .into()
}

pub(crate) fn settings_button(skin: &Skin) -> Element<'static, UiEvent> {
    let palette = skin.palette;
    let icon = container(Icon::Gear.view(skin.global_bar.gear_size, palette.text_dim))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill);
    button(icon)
        .padding(skin.global_bar.settings_padding)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(settings_button_style(skin))
        .on_press(UiEvent::OpenSettings)
        .into()
}

fn preset_chip(
    skin: &Skin,
    label: &'static str,
    preset: &'static str,
    active: bool,
) -> Element<'static, UiEvent> {
    let palette = skin.palette;
    button(
        container(
            shaped_text(label)
                .font(fonts::mono(skin.global_bar.chip_text.weight))
                .size(skin.global_bar.chip_text.size)
                .color(if active { palette.bg } else { palette.text_dim }),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill),
    )
    .padding([
        skin.global_bar.chip_padding_y,
        skin.global_bar.chip_padding_x,
    ])
    .style(preset_chip_style(skin, active))
    .on_press(UiEvent::SelectPreset(preset.to_owned()))
    .into()
}

fn preset_chip_style(
    skin: &Skin,
    active: bool,
) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle + 'static {
    let palette = skin.palette;
    let border = skin.border(skin.global_bar.chip_frame);
    move |_theme, status| {
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
            border,
            ..ButtonStyle::default()
        }
    }
}

fn settings_button_style(skin: &Skin) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle + 'static {
    let palette = skin.palette;
    let border = skin.border(skin.global_bar.settings_frame);
    move |_theme, status| {
        let background = match status {
            ButtonStatus::Hovered => palette.bg_panel_2,
            ButtonStatus::Pressed => palette.accent_soft,
            ButtonStatus::Active | ButtonStatus::Disabled => palette.bg_panel,
        };
        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color: palette.text_dim,
            border,
            ..ButtonStyle::default()
        }
    }
}
