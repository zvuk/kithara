use std::collections::BTreeMap;

use iced::{
    Alignment, Background, Border, Color, Element, Font, Length, Point, Rectangle, Renderer, Size,
    Theme,
    alignment::Vertical,
    font::Weight,
    mouse::Cursor,
    widget::{
        Canvas, Column, Row, Space, Stack, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        canvas::{self, Frame, Geometry},
        container,
        container::Style as ContainerStyle,
        row, slider,
    },
};
use kithara_ui::{
    compile::CompiledUi,
    expand::{Binding, ExpandedNode},
    ids::InternId,
    module::PropValue,
    registry::ControlCatalog,
    size::{Dim, SizeSpec},
};
use num_traits::cast::AsPrimitive;

use super::{
    ControlAction, ModularMsg,
    reads::{self, ReadValue},
};
use crate::{
    gui::{
        app::Kithara,
        fonts,
        icons::Icon,
        message::Message,
        tokens::{chrome, deck, gap, telemetry as telemetry_tokens, transport, type_scale, volume},
        typography::shaped_text,
        widgets,
        widgets::{deck_controls, global_controls, library, mini_wave, volume_meter},
    },
    theme::gui::GuiPalette,
};

const PERCENT_SCALE: f64 = 100.0;

pub(super) fn render_node<'a>(
    state: &'a Kithara,
    node: &ExpandedNode,
    ui: &'a CompiledUi,
    catalog: &dyn ControlCatalog,
) -> Element<'a, Message> {
    let p = state.palette;
    let element = match node {
        ExpandedNode::Row { children, .. } => container(
            Row::with_children(
                children
                    .iter()
                    .map(|child| render_node(state, child, ui, catalog)),
            )
            .spacing(gap::GRID)
            .width(Length::Fill),
        )
        .width(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(p.canvas.line_soft)))
        .into(),
        ExpandedNode::Column { children, .. } | ExpandedNode::Slot { children, .. } => container(
            Column::with_children(
                children
                    .iter()
                    .map(|child| render_node(state, child, ui, catalog)),
            )
            .spacing(gap::GRID)
            .width(Length::Fill),
        )
        .width(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(p.canvas.line_soft)))
        .into(),
        ExpandedNode::Control {
            path,
            kind,
            props,
            read,
            ..
        } => render_control(state, *path, *kind, props, read.as_ref(), ui),
        _ => Space::new().into(),
    };
    apply_size(element, effective_size(node, ui, catalog))
}

fn effective_size(
    node: &ExpandedNode,
    ui: &CompiledUi,
    catalog: &dyn ControlCatalog,
) -> Option<SizeSpec> {
    let declared = match node {
        ExpandedNode::Row { size, .. }
        | ExpandedNode::Column { size, .. }
        | ExpandedNode::Slot { size, .. }
        | ExpandedNode::Control { size, .. } => *size,
        _ => None,
    };
    declared.or_else(|| match node {
        ExpandedNode::Control { kind, .. } => catalog
            .kind(ui.resolve(*kind))
            .map(|description| description.size),
        _ => None,
    })
}

fn apply_size<'a>(element: Element<'a, Message>, size: Option<SizeSpec>) -> Element<'a, Message> {
    let Some(size) = size else {
        return element;
    };
    let intrinsic = element.as_widget().size_hint();
    container(element)
        .width(length_for(size.w, intrinsic.width))
        .height(length_for(size.h, intrinsic.height))
        .into()
}

fn length_for(dim: Dim, intrinsic: Length) -> Length {
    match dim {
        Dim::Fixed(value) => Length::Fixed(value),
        Dim::Range { .. } => match intrinsic {
            Length::FillPortion(_) => intrinsic,
            _ => Length::Fill,
        },
        _ => Length::Fill,
    }
}

fn render_control<'a>(
    state: &'a Kithara,
    path: InternId,
    kind: InternId,
    props: &BTreeMap<InternId, PropValue<InternId>>,
    read: Option<&Binding>,
    ui: &'a CompiledUi,
) -> Element<'a, Message> {
    let value = read.and_then(|binding| reads::resolve(&state.ui_state, binding, ui));
    let path = ui.resolve(path);
    match ui.resolve(kind) {
        "deck.header" => deck_controls::header(state, text_prop(props, "badge", ui), &value),
        "deck.summary" => deck_controls::summary(state, text_prop(props, "style", ui), &value),
        "global.brand" => global_controls::brand(state.palette),
        "global.spacer" => global_controls::spacer(state.palette),
        "preset.selector" => global_controls::preset_selector(state),
        "view.settings" => global_controls::settings_button(state.palette),
        "telemetry.bpm" => deck_controls::bpm(state, text_prop(props, "fallback", ui), &value),
        "telemetry.time" => deck_controls::time(state, &value),
        "text" => render_text(state.palette, props, &value, ui),
        "button" => render_button(state.palette, path, props, &value, ui),
        "telemetry.scalar" => render_scalar(state.palette, props, &value, ui),
        "fader.horizontal" => render_fader(state.palette, path, props, &value, ui),
        "waveform.mini" => render_waveform(state, path, props, &value, ui),
        "track_list" => library::render(state, path, &value),
        _ => Space::new().into(),
    }
}

fn render_text<'a>(
    p: GuiPalette,
    props: &BTreeMap<InternId, PropValue<InternId>>,
    value: &Option<ReadValue<'a>>,
    ui: &'a CompiledUi,
) -> Element<'a, Message> {
    let Some(ReadValue::Text(value)) = value else {
        return Space::new().into();
    };
    let track_title = text_prop(props, "style", ui) == Some("track-title");
    let (font, size) = if track_title {
        (fonts::display(Weight::Semibold), deck::TITLE_SIZE)
    } else {
        (fonts::SANS, type_scale::BODY)
    };
    shaped_text(*value)
        .font(font)
        .size(size)
        .color(p.canvas.text)
        .into()
}

fn render_button<'a>(
    p: GuiPalette,
    path: &str,
    props: &BTreeMap<InternId, PropValue<InternId>>,
    value: &Option<ReadValue<'_>>,
    ui: &'a CompiledUi,
) -> Element<'a, Message> {
    let Some(label) = text_prop(props, "label", ui) else {
        return Space::new().into();
    };
    let active = matches!(value, Some(ReadValue::Bool(true)));
    let visual = button_visual(text_prop(props, "style", ui));
    let label = if active {
        text_prop(props, "active-label", ui).unwrap_or(label)
    } else {
        label
    };
    let label_size = if visual == ButtonVisual::TransportPrimary {
        transport::PRIMARY_TEXT
    } else {
        transport::BUTTON_TEXT
    };
    let weight = if visual.is_primary() {
        Weight::Bold
    } else {
        Weight::Normal
    };
    let content: Element<'a, Message> = match visual {
        ButtonVisual::MicroPrimary => {
            let icon = if active { Icon::Pause } else { Icon::Play };
            icon.view(transport::MICRO_ICON_SIZE, p.canvas.bg)
        }
        ButtonVisual::TransportPrimary => {
            let icon = if active { Icon::Pause } else { Icon::Play };
            row![
                icon.view(transport::BUTTON_ICON_SIZE, p.canvas.bg),
                shaped_text(label)
                    .font(Font {
                        weight,
                        ..fonts::SANS
                    })
                    .size(label_size),
            ]
            .spacing(gap::INLINE_TIGHT)
            .align_y(Alignment::Center)
            .into()
        }
        ButtonVisual::Default | ButtonVisual::Transport => shaped_text(label)
            .font(Font {
                weight,
                ..fonts::SANS
            })
            .size(label_size)
            .into(),
    };
    let height = match visual {
        ButtonVisual::MicroPrimary => transport::MICRO_BUTTON_SIZE,
        _ => transport::BUTTON_HEIGHT,
    };
    let control = button(content)
        .height(Length::Fixed(height))
        .padding([0.0, transport::BUTTON_PADDING_X])
        .style(control_button_style(p, visual))
        .on_press(Message::Modular(ModularMsg::Control {
            path: path.to_owned(),
            action: ControlAction::Activate,
        }));

    match visual {
        ButtonVisual::Transport => control.width(Length::FillPortion(1)).into(),
        ButtonVisual::TransportPrimary => control.width(Length::FillPortion(2)).into(),
        ButtonVisual::MicroPrimary => control
            .width(Length::Fixed(transport::MICRO_BUTTON_SIZE))
            .into(),
        ButtonVisual::Default => control.into(),
    }
}

fn render_scalar<'a>(
    p: GuiPalette,
    props: &BTreeMap<InternId, PropValue<InternId>>,
    value: &Option<ReadValue<'_>>,
    ui: &'a CompiledUi,
) -> Element<'a, Message> {
    let Some(ReadValue::Scalar(value)) = value else {
        return Space::new().into();
    };
    let formatted = if text_prop(props, "format", ui) == Some("percent") {
        format!("{:>3.0}%", *value * PERCENT_SCALE)
    } else {
        format!("{value:.2}")
    };
    container(
        shaped_text(formatted)
            .font(fonts::MONO)
            .size(telemetry_tokens::SCALAR_TEXT_SIZE)
            .color(p.canvas.text),
    )
    .padding([
        telemetry_tokens::SCALAR_PADDING_Y,
        telemetry_tokens::SCALAR_PADDING_X,
    ])
    .style(move |_| {
        ContainerStyle::default()
            .background(Background::Color(p.canvas.bg_inset))
            .border(Border::default().width(1).color(p.canvas.line))
    })
    .into()
}

fn render_fader<'a>(
    p: GuiPalette,
    path: &str,
    props: &BTreeMap<InternId, PropValue<InternId>>,
    value: &Option<ReadValue<'_>>,
    ui: &'a CompiledUi,
) -> Element<'a, Message> {
    let Some(ReadValue::Scalar(value)) = value else {
        return Space::new().into();
    };
    if matches!(
        text_prop(props, "style", ui),
        Some("volume" | "volume-compact")
    ) {
        return render_segmented_volume(p, path, *value);
    }

    let path = path.to_owned();
    let slider = slider(0.0..=1.0, value.clamp(0.0, 1.0), move |value| {
        Message::Modular(ModularMsg::Control {
            path: path.clone(),
            action: ControlAction::SetScalar(value),
        })
    })
    .step(volume::STEP)
    .height(volume::SLIDER_HEIGHT)
    .style(widgets::slider_style(p))
    .width(Length::Fill);
    let ticks = Canvas::new(FaderTicks {
        color: p.canvas.line_soft,
    })
    .height(Length::Fixed(volume::TICKS_HEIGHT))
    .width(Length::Fill);
    let slider = container(slider)
        .height(Length::Fixed(volume::TICKS_HEIGHT))
        .align_y(Vertical::Top);
    let rail = Stack::with_children([ticks.into(), slider.into()])
        .height(Length::Fixed(volume::TICKS_HEIGHT))
        .width(Length::Fill);
    let label: Element<'_, Message> = shaped_text("VOL")
        .font(fonts::SANS)
        .size(type_scale::MICRO_LABEL)
        .color(p.canvas.muted)
        .into();
    let control = row![
        container(label).width(Length::Fixed(volume::LABEL_WIDTH)),
        rail,
    ]
    .align_y(Alignment::Center)
    .height(Length::Fixed(volume::CONTROL_HEIGHT))
    .width(Length::Fill);

    container(control)
        .padding([0.0, volume::CONTROL_PADDING_X])
        .height(Length::Fixed(volume::CONTROL_HEIGHT))
        .width(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(p.canvas.bg_panel)))
        .into()
}

fn render_segmented_volume(p: GuiPalette, path: &str, value: f64) -> Element<'static, Message> {
    let control = row![
        container(Icon::Volume.view(volume::ICON_SIZE, p.canvas.muted))
            .width(Length::Fixed(volume::LABEL_WIDTH)),
        volume_meter::view(p, path.to_owned(), value),
    ]
    .spacing(volume::CONTENT_GAP)
    .align_y(Alignment::Center)
    .height(Length::Fixed(volume::CONTROL_HEIGHT))
    .width(Length::Fill);

    container(control)
        .padding([0.0, volume::CONTROL_PADDING_X])
        .height(Length::Fixed(volume::CONTROL_HEIGHT))
        .width(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(p.canvas.bg_panel)))
        .into()
}

fn render_waveform<'a>(
    state: &'a Kithara,
    path: &str,
    props: &BTreeMap<InternId, PropValue<InternId>>,
    value: &Option<ReadValue<'a>>,
    ui: &'a CompiledUi,
) -> Element<'a, Message> {
    let analysis = match value {
        Some(ReadValue::Waveform(analysis)) => *analysis,
        _ => None,
    };
    let progress: f32 = reads::position_normalized(&state.ui_state).as_();
    mini_wave::view(
        analysis,
        progress,
        state.palette,
        path.to_owned(),
        Length::Fill,
        text_prop(props, "style", ui) == Some("hero"),
    )
}

fn text_prop<'a>(
    props: &BTreeMap<InternId, PropValue<InternId>>,
    name: &str,
    ui: &'a CompiledUi,
) -> Option<&'a str> {
    props
        .iter()
        .find(|(key, _)| ui.resolve(**key) == name)
        .and_then(|(_, value)| match value {
            PropValue::Text(value) => Some(ui.resolve(*value)),
            _ => None,
        })
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ButtonVisual {
    Default,
    Transport,
    TransportPrimary,
    MicroPrimary,
}

impl ButtonVisual {
    fn is_primary(self) -> bool {
        matches!(self, Self::TransportPrimary | Self::MicroPrimary)
    }
}

fn button_visual(style: Option<&str>) -> ButtonVisual {
    match style {
        Some("transport") => ButtonVisual::Transport,
        Some("transport-primary") => ButtonVisual::TransportPrimary,
        Some("micro-primary") => ButtonVisual::MicroPrimary,
        _ => ButtonVisual::Default,
    }
}

fn control_button_style(
    p: GuiPalette,
    visual: ButtonVisual,
) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle {
    move |_theme, status| {
        let highlighted = visual.is_primary();
        let background = if highlighted {
            match status {
                ButtonStatus::Hovered => p.accent_strong,
                ButtonStatus::Pressed => p.accent_soft,
                ButtonStatus::Active | ButtonStatus::Disabled => p.accent,
            }
        } else {
            match status {
                ButtonStatus::Hovered => p.canvas.bg_panel_2,
                ButtonStatus::Pressed => p.accent_soft,
                ButtonStatus::Active | ButtonStatus::Disabled => p.canvas.bg_panel,
            }
        };
        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color: if highlighted {
                p.canvas.bg
            } else {
                p.canvas.text
            },
            border: Border::default(),
            ..ButtonStyle::default()
        }
    }
}

struct FaderTicks {
    color: Color,
}

impl canvas::Program<Message> for FaderTicks {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let mut x = 0.0;
        let y = (bounds.height - volume::TICK_HEIGHT).max(0.0);
        while x <= bounds.width {
            frame.fill_rectangle(
                Point::new(x, y),
                Size::new(chrome::BORDER_WIDTH, volume::TICK_HEIGHT),
                self.color,
            );
            x += volume::TICK_STEP;
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn fixed_size_spec_sets_both_element_axes() {
        let element: Element<'static, Message> = Space::new().into();
        let element = apply_size(
            element,
            Some(SizeSpec::new(Dim::Fixed(34.0), Dim::Fixed(6.0))),
        );

        assert_eq!(
            element.as_widget().size(),
            Size::new(Length::Fixed(34.0), Length::Fixed(6.0))
        );
    }

    #[kithara::test]
    fn range_preserves_widget_fill_portion() {
        let element: Element<'static, Message> = Space::new()
            .width(Length::FillPortion(2))
            .height(Length::Fill)
            .into();
        let element = apply_size(
            element,
            Some(SizeSpec::new(
                Dim::Range {
                    min: 20.0,
                    max: None,
                },
                Dim::Fill,
            )),
        );

        assert_eq!(
            element.as_widget().size(),
            Size::new(Length::FillPortion(2), Length::Fill)
        );
    }
}
