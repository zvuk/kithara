use iced::{
    Alignment, Background, Element, Length, Theme,
    widget::{
        button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        container,
        container::Style as ContainerStyle,
        row,
    },
};

use crate::{
    builtin,
    render::{IcedSkin, ReadValue, Reads, Skin, UiEvent, fonts, shaped_text},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct PresetSelector<'reads, 'skin> {
    skin: &'skin Skin,
    reads: &'reads dyn Reads,
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
                .background(Background::Color(palette.line.into()))
                .border(selector_border)
        }))
        .padding([
            self.skin.global_bar.selector_padding_y,
            self.skin.global_bar.selector_padding_x,
        ])
        .width(Length::Fixed(self.skin.global_bar.selector_width))
        .height(Length::Fixed(self.skin.global_bar.height))
        .center_y(Length::Fill)
        .style(move |_| {
            ContainerStyle::default().background(Background::Color(palette.bg_panel.into()))
        })
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
            ButtonStatus::Hovered if active => palette.accent_strong.into(),
            ButtonStatus::Hovered => palette.bg_panel_2.into(),
            ButtonStatus::Pressed => palette.accent_soft.into(),
            ButtonStatus::Active | ButtonStatus::Disabled if active => palette.accent.into(),
            ButtonStatus::Active | ButtonStatus::Disabled => palette.bg_panel.into(),
        };
        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color: if active {
                palette.bg.into()
            } else {
                palette.text_dim.into()
            },
            border,
            ..ButtonStyle::default()
        }
    }
}
