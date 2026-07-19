use iced::{
    Alignment, Background, Border, Element, Font, Length, Theme,
    font::Weight,
    widget::{
        button,
        button::{Status as ButtonStatus, Style as IcedButtonStyle},
        container, row,
    },
};

use crate::{
    module::ButtonStyle,
    render::{ControlAction, Icon, ReadValue, RenderPalette, UiEvent, fonts, shaped_text},
};

struct Consts;

impl Consts {
    const BORDER_WIDTH: f32 = 1.0;
    const BUTTON_HEIGHT: f32 = 28.0;
    const BUTTON_ICON_SIZE: f32 = 11.0;
    const BUTTON_PADDING_X: f32 = 8.0;
    const BUTTON_TEXT: f32 = 11.0;
    const ICON_GAP: f32 = 4.0;
    const MICRO_BUTTON_SIZE: f32 = 34.0;
    const MICRO_ICON_SIZE: f32 = 14.0;
}

pub(crate) fn view<'a>(
    path: &str,
    label: &'a str,
    active_label: Option<&'a str>,
    style: ButtonStyle,
    value: Option<&ReadValue<'_>>,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let active = matches!(value, Some(ReadValue::Bool(true)));
    let label = if active {
        active_label.unwrap_or(label)
    } else {
        label
    };
    let label_size = Consts::BUTTON_TEXT;
    let weight = if is_primary(style) {
        Weight::Bold
    } else {
        Weight::Normal
    };
    let content: Element<'a, UiEvent> = match style {
        ButtonStyle::MicroPrimary => {
            let icon = if active { Icon::Pause } else { Icon::Play };
            icon.view(Consts::MICRO_ICON_SIZE, palette.bg)
        }
        ButtonStyle::Transport => transport_content(label, label_size, weight, palette),
        ButtonStyle::TransportPrimary | ButtonStyle::Default => shaped_text(label)
            .font(Font {
                weight,
                ..fonts::MONO
            })
            .size(label_size)
            .into(),
    };
    let height = if style == ButtonStyle::MicroPrimary {
        Consts::MICRO_BUTTON_SIZE
    } else {
        Consts::BUTTON_HEIGHT
    };
    let centered = container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill);
    let control = button(centered)
        .height(Length::Fixed(height))
        .padding([0.0, Consts::BUTTON_PADDING_X])
        .style(control_button_style(palette, style))
        .on_press(UiEvent::Control {
            path: path.to_owned(),
            action: ControlAction::Activate,
        });

    match style {
        ButtonStyle::Transport => control.width(Length::FillPortion(1)).into(),
        ButtonStyle::TransportPrimary => control.width(Length::FillPortion(2)).into(),
        ButtonStyle::MicroPrimary => control
            .width(Length::Fixed(Consts::MICRO_BUTTON_SIZE))
            .into(),
        ButtonStyle::Default => control.width(Length::Shrink).into(),
    }
}

fn transport_content<'a>(
    label: &'a str,
    size: f32,
    weight: Weight,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let icon = match label {
        "PREV" => Some(Icon::SkipBack),
        "NEXT" => Some(Icon::SkipForward),
        _ => None,
    };
    icon.map_or_else(
        || {
            shaped_text(label)
                .font(Font {
                    weight,
                    ..fonts::MONO
                })
                .size(size)
                .into()
        },
        |icon| icon_label(icon, label, size, weight, palette.text),
    )
}

fn icon_label<'a>(
    icon: Icon,
    label: &'a str,
    size: f32,
    weight: Weight,
    color: iced::Color,
) -> Element<'a, UiEvent> {
    row![
        icon.view(Consts::BUTTON_ICON_SIZE, color),
        shaped_text(label)
            .font(Font {
                weight,
                ..fonts::MONO
            })
            .size(size),
    ]
    .spacing(Consts::ICON_GAP)
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
    palette: RenderPalette,
    style: ButtonStyle,
) -> impl Fn(&Theme, ButtonStatus) -> IcedButtonStyle {
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
        let border = if highlighted {
            Border::default()
        } else {
            Border::default()
                .width(Consts::BORDER_WIDTH)
                .color(palette.line)
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
