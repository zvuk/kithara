use iced::{
    Alignment, Background, Element, Length, Theme,
    widget::{
        button,
        button::{Status as ButtonStatus, Style as IcedButtonStyle},
        container, row,
    },
};

use crate::{
    module::ButtonStyle,
    render::{ControlAction, Icon, ReadValue, Skin, UiEvent, fonts, shaped_text},
    skin::FontSkin,
};

pub(crate) fn view<'a>(
    path: &str,
    label: &'a str,
    active_label: Option<&'a str>,
    style: ButtonStyle,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let active = matches!(value, Some(ReadValue::Bool(true)));
    let label = if active {
        active_label.unwrap_or(label)
    } else {
        label
    };
    let font = if is_primary(style) {
        skin.button.primary_text
    } else {
        skin.button.text
    };
    let palette = skin.palette;
    let content: Element<'a, UiEvent> = match style {
        ButtonStyle::MicroPrimary => {
            let icon = if active { Icon::Pause } else { Icon::Play };
            icon.view(skin.button.micro_icon_size, palette.bg)
        }
        ButtonStyle::Transport => transport_content(label, font, skin),
        ButtonStyle::TransportPrimary | ButtonStyle::Default => shaped_text(label)
            .font(fonts::mono(font.weight))
            .size(font.size)
            .into(),
    };
    let height = if style == ButtonStyle::MicroPrimary {
        skin.button.micro_size
    } else {
        skin.button.height
    };
    let centered = container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill);
    let control = button(centered)
        .height(Length::Fixed(height))
        .padding([skin.button.padding_y, skin.button.padding_x])
        .style(control_button_style(skin, style))
        .on_press(UiEvent::Control {
            path: path.to_owned(),
            action: ControlAction::Activate,
        });

    match style {
        ButtonStyle::Transport => control
            .width(Length::FillPortion(skin.button.transport_fill))
            .into(),
        ButtonStyle::TransportPrimary => control
            .width(Length::FillPortion(skin.button.primary_fill))
            .into(),
        ButtonStyle::MicroPrimary => control.width(Length::Fixed(skin.button.micro_size)).into(),
        ButtonStyle::Default => control.width(Length::Shrink).into(),
    }
}

fn transport_content<'a>(label: &'a str, font: FontSkin, skin: &Skin) -> Element<'a, UiEvent> {
    let icon = match label {
        "PREV" => Some(Icon::SkipBack),
        "NEXT" => Some(Icon::SkipForward),
        _ => None,
    };
    icon.map_or_else(
        || {
            shaped_text(label)
                .font(fonts::mono(font.weight))
                .size(font.size)
                .into()
        },
        |icon| icon_label(icon, label, font, skin),
    )
}

fn icon_label<'a>(icon: Icon, label: &'a str, font: FontSkin, skin: &Skin) -> Element<'a, UiEvent> {
    row![
        icon.view(skin.button.icon_size, skin.palette.text),
        shaped_text(label)
            .font(fonts::mono(font.weight))
            .size(font.size),
    ]
    .spacing(skin.button.icon_gap)
    .align_y(Alignment::Center)
    .into()
}

fn is_primary(style: ButtonStyle) -> bool {
    matches!(
        style,
        ButtonStyle::TransportPrimary | ButtonStyle::MicroPrimary
    )
}

fn control_button_style(
    skin: &Skin,
    style: ButtonStyle,
) -> impl Fn(&Theme, ButtonStatus) -> IcedButtonStyle + 'static {
    let palette = skin.palette;
    let border = skin.border(if is_primary(style) {
        skin.button.primary_frame
    } else {
        skin.button.frame
    });
    move |_theme, status| {
        let highlighted = is_primary(style);
        let background = if highlighted {
            match status {
                ButtonStatus::Hovered => palette.accent_strong,
                ButtonStatus::Pressed => palette.accent_soft,
                ButtonStatus::Active | ButtonStatus::Disabled => palette.accent,
            }
        } else {
            match status {
                ButtonStatus::Hovered => palette.bg_panel_2,
                ButtonStatus::Pressed => palette.accent_soft,
                ButtonStatus::Active | ButtonStatus::Disabled => palette.bg_panel,
            }
        };
        IcedButtonStyle {
            background: Some(Background::Color(background)),
            text_color: if highlighted {
                palette.bg
            } else {
                palette.text
            },
            border,
            ..IcedButtonStyle::default()
        }
    }
}
