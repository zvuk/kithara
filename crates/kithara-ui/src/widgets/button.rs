use iced::{
    Alignment, Background, Border, Element, Font, Length, Theme,
    font::Weight,
    widget::{
        Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        container, row,
    },
};

use crate::{
    registry::{ControlKindDesc, PropKind, ValueKind},
    render::{ControlAction, Icon, ReadValue, RenderPalette, UiEvent, fonts, shaped_text},
    size::{Dim, SizeSpec},
};

struct Consts;

impl Consts {
    const BUTTON_HEIGHT: f32 = 28.0;
    const BUTTON_ICON_SIZE: f32 = 11.0;
    const BUTTON_MIN_WIDTH: f32 = 72.0;
    const BUTTON_PADDING_X: f32 = 10.0;
    const BUTTON_TEXT: f32 = 13.0;
    const ICON_GAP: f32 = 4.0;
    const MICRO_BUTTON_SIZE: f32 = 34.0;
    const MICRO_ICON_SIZE: f32 = 14.0;
    const PRIMARY_TEXT: f32 = 14.0;
}

pub(crate) fn desc() -> ControlKindDesc {
    ControlKindDesc::new(Some(ValueKind::Bool), Some(ValueKind::Trigger))
        .with_prop("label", PropKind::Text)
        .with_prop("active-label", PropKind::Text)
        .with_prop("style", PropKind::Text)
        .with_size(SizeSpec::new(
            Dim::Range {
                min: Consts::BUTTON_MIN_WIDTH,
                max: None,
            },
            Dim::Fixed(Consts::BUTTON_HEIGHT),
        ))
}

pub(crate) fn view<'a>(
    path: &str,
    label: Option<&'a str>,
    active_label: Option<&'a str>,
    style: Option<&str>,
    value: Option<&ReadValue<'_>>,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let Some(label) = label else {
        return Space::new().into();
    };
    let active = matches!(value, Some(ReadValue::Bool(true)));
    let visual = ButtonVisual::from(style);
    let label = if active {
        active_label.unwrap_or(label)
    } else {
        label
    };
    let label_size = if visual == ButtonVisual::TransportPrimary {
        Consts::PRIMARY_TEXT
    } else {
        Consts::BUTTON_TEXT
    };
    let weight = if visual.is_primary() {
        Weight::Bold
    } else {
        Weight::Normal
    };
    let content: Element<'a, UiEvent> = match visual {
        ButtonVisual::MicroPrimary => {
            let icon = if active { Icon::Pause } else { Icon::Play };
            icon.view(Consts::MICRO_ICON_SIZE, palette.bg)
        }
        ButtonVisual::TransportPrimary => {
            let icon = if active { Icon::Pause } else { Icon::Play };
            icon_label(icon, label, label_size, weight, palette.bg)
        }
        ButtonVisual::Transport => transport_content(label, label_size, weight, palette),
        ButtonVisual::Default => shaped_text(label)
            .font(Font {
                weight,
                ..fonts::SANS
            })
            .size(label_size)
            .into(),
    };
    let height = if visual == ButtonVisual::MicroPrimary {
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
        .style(control_button_style(palette, visual))
        .on_press(UiEvent::Control {
            path: path.to_owned(),
            action: ControlAction::Activate,
        });

    match visual {
        ButtonVisual::Transport => control.width(Length::FillPortion(1)).into(),
        ButtonVisual::TransportPrimary => control.width(Length::FillPortion(2)).into(),
        ButtonVisual::MicroPrimary => control
            .width(Length::Fixed(Consts::MICRO_BUTTON_SIZE))
            .into(),
        ButtonVisual::Default => control.width(Length::Shrink).into(),
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
                    ..fonts::SANS
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
                ..fonts::SANS
            })
            .size(size),
    ]
    .spacing(Consts::ICON_GAP)
    .align_y(Alignment::Center)
    .into()
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ButtonVisual {
    Default,
    Transport,
    TransportPrimary,
    MicroPrimary,
}

impl From<Option<&str>> for ButtonVisual {
    fn from(style: Option<&str>) -> Self {
        match style {
            Some("transport") => Self::Transport,
            Some("transport-primary") => Self::TransportPrimary,
            Some("micro-primary") => Self::MicroPrimary,
            _ => Self::Default,
        }
    }
}

impl ButtonVisual {
    fn is_primary(self) -> bool {
        matches!(self, Self::TransportPrimary | Self::MicroPrimary)
    }
}

fn control_button_style(
    palette: RenderPalette,
    visual: ButtonVisual,
) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle {
    move |_theme, status| {
        let highlighted = visual.is_primary();
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
        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color: if highlighted {
                palette.bg
            } else {
                palette.text
            },
            border: Border::default(),
            ..ButtonStyle::default()
        }
    }
}
