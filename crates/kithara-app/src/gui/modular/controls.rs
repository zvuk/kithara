use std::collections::BTreeMap;

use iced::{
    Background, Border, Element, Length, Theme,
    font::Weight,
    widget::{
        Column, Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        container,
        container::Style as ContainerStyle,
        scrollable, slider, text,
    },
};
use kithara_queue::TrackEntry;
use kithara_ui::{
    compile::CompiledUi,
    expand::{Binding, ExpandedNode},
    ids::InternId,
    module::PropValue,
};
use num_traits::cast::AsPrimitive;

use super::{
    ControlAction, ModularMsg, mini_wave,
    reads::{self, ReadValue},
};
use crate::{
    gui::{app::Kithara, fonts, message::Message, tokens::gap, widgets},
    theme::gui::GuiPalette,
};

pub(super) fn render_node<'a>(
    state: &'a Kithara,
    node: &ExpandedNode,
    ui: &'a CompiledUi,
) -> Element<'a, Message> {
    match node {
        ExpandedNode::Row { children, .. } => {
            Row::with_children(children.iter().map(|child| render_node(state, child, ui)))
                .spacing(gap::INLINE)
                .width(Length::Fill)
                .into()
        }
        ExpandedNode::Column { children, .. } | ExpandedNode::Slot { children, .. } => {
            Column::with_children(children.iter().map(|child| render_node(state, child, ui)))
                .spacing(gap::INLINE)
                .width(Length::Fill)
                .into()
        }
        ExpandedNode::Control {
            path,
            kind,
            props,
            read,
            ..
        } => render_control(state, *path, *kind, props, read.as_ref(), ui),
        _ => Space::new().into(),
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
        "text" => render_text(state.palette, props, value, ui),
        "button" => render_button(state.palette, path, props, &value, ui),
        "telemetry.scalar" => render_scalar(state.palette, props, &value, ui),
        "fader.horizontal" => render_fader(state.palette, path, &value),
        "waveform.mini" => render_waveform(state, path, &value),
        "track_list" => render_track_list(state, path, &value),
        _ => Space::new().into(),
    }
}

fn render_text<'a>(
    p: GuiPalette,
    props: &BTreeMap<InternId, PropValue<InternId>>,
    value: Option<ReadValue<'_>>,
    ui: &'a CompiledUi,
) -> Element<'a, Message> {
    let Some(ReadValue::Text(value)) = value else {
        return Space::new().into();
    };
    let track_title = text_prop(props, "style", ui) == Some("track-title");
    let (font, size) = if track_title {
        (fonts::display(Weight::Semibold), 16.0)
    } else {
        (fonts::SANS, 13.0)
    };
    text(value).font(font).size(size).color(p.text).into()
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
    let Some(ReadValue::Bool(active)) = value else {
        return Space::new().into();
    };
    button(text(label.to_owned()).font(fonts::SANS).size(13.0))
        .padding([6, 10])
        .style(control_button_style(p, *active))
        .on_press(Message::Modular(ModularMsg::Control {
            path: path.to_owned(),
            action: ControlAction::Activate,
        }))
        .into()
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
        format!("{:>3.0}%", *value * 100.0)
    } else {
        format!("{value:.2}")
    };
    container(text(formatted).font(fonts::MONO).size(12.0).color(p.text))
        .padding([4, 7])
        .style(move |_| {
            ContainerStyle::default()
                .background(Background::Color(p.bg_inset))
                .border(Border::default().width(1).color(p.line))
        })
        .into()
}

fn render_fader<'a>(
    p: GuiPalette,
    path: &str,
    value: &Option<ReadValue<'_>>,
) -> Element<'a, Message> {
    let Some(ReadValue::Scalar(value)) = value else {
        return Space::new().into();
    };
    let path = path.to_owned();
    slider(0.0..=1.0, value.clamp(0.0, 1.0), move |value| {
        Message::Modular(ModularMsg::Control {
            path: path.clone(),
            action: ControlAction::SetScalar(value),
        })
    })
    .step(0.01)
    .style(widgets::slider_style(p))
    .width(Length::Fill)
    .into()
}

fn render_waveform<'a>(
    state: &'a Kithara,
    path: &str,
    value: &Option<ReadValue<'a>>,
) -> Element<'a, Message> {
    let Some(ReadValue::Waveform(analysis)) = value else {
        return Space::new().height(Length::Fixed(28.0)).into();
    };
    let progress: f32 = reads::position_normalized(&state.ui_state).as_();
    mini_wave::view(*analysis, progress, state.palette, path.to_owned())
}

fn render_track_list<'a>(
    state: &'a Kithara,
    path: &str,
    value: &Option<ReadValue<'a>>,
) -> Element<'a, Message> {
    let Some(ReadValue::Tracks(tracks)) = value else {
        return Space::new().into();
    };
    track_list(state, path, tracks)
}

fn track_list<'a>(state: &'a Kithara, path: &str, tracks: &[TrackEntry]) -> Element<'a, Message> {
    let p = state.palette;
    let rows = tracks.iter().enumerate().map(|(index, track)| {
        let current = state.ui_state.current_track_index == Some(index);
        let marker = if current { "*" } else { " " };
        let path = path.to_owned();
        button(
            Row::with_children([
                text(marker)
                    .font(fonts::MONO)
                    .size(11.0)
                    .color(p.accent)
                    .into(),
                text(format!("{:02}", index + 1))
                    .font(fonts::MONO)
                    .size(11.0)
                    .color(p.muted)
                    .into(),
                text(track.name.clone())
                    .font(fonts::SANS)
                    .size(13.0)
                    .color(p.text)
                    .width(Length::Fill)
                    .into(),
            ])
            .spacing(gap::INLINE_WIDE)
            .width(Length::Fill),
        )
        .padding([6, 8])
        .width(Length::Fill)
        .style(track_button_style(p, current))
        .on_press(Message::Modular(ModularMsg::Control {
            path,
            action: ControlAction::SelectIndex(index),
        }))
        .into()
    });

    scrollable(Column::with_children(rows).spacing(1.0).width(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
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

fn control_button_style(
    p: GuiPalette,
    active: bool,
) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle {
    move |_theme, status| {
        let background = if active {
            p.accent
        } else {
            match status {
                ButtonStatus::Hovered => p.bg_panel_2,
                ButtonStatus::Pressed => p.accent_soft,
                ButtonStatus::Active | ButtonStatus::Disabled => p.bg_inset,
            }
        };
        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color: if active { p.bg_deep } else { p.text },
            border: Border::default().width(1).color(p.line),
            ..ButtonStyle::default()
        }
    }
}

fn track_button_style(
    p: GuiPalette,
    current: bool,
) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle {
    move |_theme, status| {
        let background = match status {
            ButtonStatus::Pressed => p.accent_soft,
            ButtonStatus::Hovered if !current => p.bg_inset,
            _ if current => p.bg_panel_2,
            ButtonStatus::Active | ButtonStatus::Hovered | ButtonStatus::Disabled => p.bg_panel,
        };
        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color: p.text,
            border: Border::default(),
            ..ButtonStyle::default()
        }
    }
}
