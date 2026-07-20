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
    widgets::Widget,
};

const BRAND_LETTERS: [&str; 7] = ["K", "I", "T", "H", "A", "R", "A"];

#[derive(bon::Builder)]
pub(crate) struct Brand<'skin> {
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Brand<'_> {
    fn view(self) -> Element<'a, UiEvent> {
        let palette = self.skin.palette;
        let letters = BRAND_LETTERS.into_iter().map(|letter| {
            shaped_text(letter)
                .font(fonts::display(self.skin.global_bar.brand_text.weight))
                .size(self.skin.global_bar.brand_text.size)
                .color(palette.text)
                .into()
        });
        container(
            Row::with_children(letters)
                .spacing(self.skin.global_bar.brand_gap)
                .align_y(Alignment::Center),
        )
        .padding([
            self.skin.global_bar.brand_padding_y,
            self.skin.global_bar.brand_padding_x,
        ])
        .width(Length::Fixed(self.skin.global_bar.brand_width))
        .height(Length::Fixed(self.skin.global_bar.height))
        .align_y(Vertical::Center)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
        .into()
    }
}

#[derive(bon::Builder)]
pub(crate) struct PresetSelector<'reads, 'skin> {
    reads: &'reads dyn Reads,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for PresetSelector<'_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let palette = self.skin.palette;
        let active = match self.reads.get("ui.preset") {
            Some(ReadValue::Text(preset)) => preset,
            _ => "",
        };
        let chips = row![
            PresetChip::builder()
                .skin(self.skin)
                .label("MICRO")
                .preset(builtin::MICRO_PRESET)
                .active(active == builtin::MICRO_PRESET)
                .build()
                .view(),
            PresetChip::builder()
                .skin(self.skin)
                .label("PLAYER")
                .preset(builtin::PLAYER_PRESET)
                .active(active == builtin::PLAYER_PRESET)
                .build()
                .view(),
        ]
        .spacing(self.skin.global_bar.chip_gap)
        .align_y(Alignment::Center);
        let selector_border = self.skin.border(self.skin.global_bar.selector_frame);
        container(container(chips).style(move |_| {
            ContainerStyle::default()
                .background(Background::Color(palette.line))
                .border(selector_border)
        }))
        .padding([
            self.skin.global_bar.selector_padding_y,
            self.skin.global_bar.selector_padding_x,
        ])
        .width(Length::Fixed(self.skin.global_bar.selector_width))
        .height(Length::Fixed(self.skin.global_bar.height))
        .center_y(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)))
        .into()
    }
}

#[derive(bon::Builder)]
pub(crate) struct Spacer<'skin> {
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Spacer<'_> {
    fn view(self) -> Element<'a, UiEvent> {
        let palette = self.skin.palette;
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(self.skin.global_bar.height))
            .align_y(Vertical::Center)
            .style(move |_| {
                ContainerStyle::default().background(Background::Color(palette.bg_panel))
            })
            .into()
    }
}

#[derive(bon::Builder)]
pub(crate) struct SettingsButton<'skin> {
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for SettingsButton<'_> {
    fn view(self) -> Element<'a, UiEvent> {
        let palette = self.skin.palette;
        let icon = container(Icon::Gear.view(self.skin.global_bar.gear_size, palette.text_dim))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        button(icon)
            .padding(self.skin.global_bar.settings_padding)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(settings_button_style(self.skin))
            .on_press(UiEvent::OpenSettings)
            .into()
    }
}

#[derive(bon::Builder)]
struct PresetChip<'skin> {
    skin: &'skin Skin,
    label: &'static str,
    preset: &'static str,
    active: bool,
}

impl<'a> Widget<'a> for PresetChip<'_> {
    fn view(self) -> Element<'a, UiEvent> {
        let palette = self.skin.palette;
        button(
            container(
                shaped_text(self.label)
                    .font(fonts::mono(self.skin.global_bar.chip_text.weight))
                    .size(self.skin.global_bar.chip_text.size)
                    .color(if self.active {
                        palette.bg
                    } else {
                        palette.text_dim
                    }),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill),
        )
        .padding([
            self.skin.global_bar.chip_padding_y,
            self.skin.global_bar.chip_padding_x,
        ])
        .style(preset_chip_style(self.skin, self.active))
        .on_press(UiEvent::SelectPreset(self.preset.to_owned()))
        .into()
    }
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
