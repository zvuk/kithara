use iced::{
    Alignment, Background, Border, Element, Length, Theme,
    font::Weight,
    widget::{
        Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        container,
        container::Style as ContainerStyle,
        row,
    },
};
use kithara_ui::builtin;

use crate::{
    gui::{
        app::Kithara,
        fonts,
        icons::Icon,
        message::Message,
        modular::ModularMsg,
        tokens::{chrome, gap, global_bar},
        typography::shaped_text,
    },
    theme::gui::GuiPalette,
};

const BRAND_LETTERS: [&str; 7] = ["K", "I", "T", "H", "A", "R", "A"];

pub(crate) fn brand(p: GuiPalette) -> Element<'static, Message> {
    let letters = BRAND_LETTERS.into_iter().map(|letter| {
        shaped_text(letter)
            .font(fonts::display(Weight::Bold))
            .size(global_bar::BRAND_SIZE)
            .color(p.canvas.text)
            .into()
    });
    container(
        Row::with_children(letters)
            .spacing(global_bar::BRAND_GAP)
            .align_y(Alignment::Center),
    )
    .padding([0.0, global_bar::BRAND_PADDING_X])
    .width(Length::Fixed(global_bar::BRAND_WIDTH))
    .height(Length::Fixed(global_bar::HEIGHT))
    .style(move |_| ContainerStyle::default().background(Background::Color(p.canvas.bg_panel)))
    .into()
}

pub(crate) fn preset_selector(state: &Kithara) -> Element<'static, Message> {
    let p = state.palette;
    let chips = row![
        preset_chip(
            p,
            "MICRO",
            builtin::MICRO_PRESET,
            state.modular.preset == builtin::MICRO_PRESET,
        ),
        preset_chip(
            p,
            "PLAYER",
            builtin::PLAYER_PRESET,
            state.modular.preset == builtin::PLAYER_PRESET,
        ),
    ]
    .spacing(gap::GRID)
    .align_y(Alignment::Center);

    container(container(chips).style(move |_| {
        ContainerStyle::default()
            .background(Background::Color(p.canvas.line))
            .border(
                Border::default()
                    .width(chrome::BORDER_WIDTH)
                    .color(p.canvas.line),
            )
    }))
    .padding([0.0, global_bar::SELECTOR_PADDING_X])
    .width(Length::Fixed(global_bar::SELECTOR_WIDTH))
    .height(Length::Fixed(global_bar::HEIGHT))
    .center_y(Length::Fill)
    .style(move |_| ContainerStyle::default().background(Background::Color(p.canvas.bg_panel)))
    .into()
}

pub(crate) fn spacer(p: GuiPalette) -> Element<'static, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(global_bar::HEIGHT))
        .style(move |_| ContainerStyle::default().background(Background::Color(p.canvas.bg_panel)))
        .into()
}

pub(crate) fn settings_button(p: GuiPalette) -> Element<'static, Message> {
    button(Icon::Settings.view(global_bar::GEAR_SIZE, p.canvas.text_dim))
        .padding(0.0)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |theme, status| settings_button_style(p, theme, status))
        .on_press(Message::Modular(ModularMsg::OpenSettings))
        .into()
}

fn preset_chip(
    p: GuiPalette,
    label: &'static str,
    preset: &'static str,
    active: bool,
) -> Element<'static, Message> {
    button(
        shaped_text(label)
            .font(fonts::MONO)
            .size(global_bar::CHIP_TEXT)
            .color(if active {
                p.canvas.bg
            } else {
                p.canvas.text_dim
            }),
    )
    .padding([global_bar::CHIP_PADDING_Y, global_bar::CHIP_PADDING_X])
    .style(move |theme, status| preset_chip_style(p, active, theme, status))
    .on_press(Message::Modular(ModularMsg::SelectPreset(
        preset.to_owned(),
    )))
    .into()
}

fn preset_chip_style(
    p: GuiPalette,
    active: bool,
    _theme: &Theme,
    status: ButtonStatus,
) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered if active => p.accent_strong,
        ButtonStatus::Hovered => p.canvas.bg_panel_2,
        ButtonStatus::Pressed => p.accent_soft,
        ButtonStatus::Active | ButtonStatus::Disabled if active => p.accent,
        ButtonStatus::Active | ButtonStatus::Disabled => p.canvas.bg_panel,
    };
    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: if active {
            p.canvas.bg
        } else {
            p.canvas.text_dim
        },
        ..ButtonStyle::default()
    }
}

fn settings_button_style(p: GuiPalette, _theme: &Theme, status: ButtonStatus) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Hovered => p.canvas.bg_panel_2,
        ButtonStatus::Pressed => p.accent_soft,
        ButtonStatus::Active | ButtonStatus::Disabled => p.canvas.bg_panel,
    };
    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: p.canvas.text_dim,
        ..ButtonStyle::default()
    }
}
