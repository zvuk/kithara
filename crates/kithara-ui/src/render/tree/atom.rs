use iced::Element;

use super::icon::render_icon;
use crate::{
    atoms::{
        chip::Chip,
        design::{
            cell::Cell, crossfader::Crossfader, segmented::Segmented, select::Select,
            status_dot::StatusDot, swatch::Swatch,
        },
        knob::Knob,
        readout::Readout,
    },
    compile::CompiledUi,
    ids::InternId,
    module::{ChipStyle, FaderStyle, GlyphStyle, IconName, Tone},
    render::{ReadValue, Skin, UiEvent},
    skin::ColorRole,
    widgets::{
        Widget,
        fader::Fader,
        nav::{Glyph, NavItem, TabLarge},
    },
};

pub(super) fn crossfader<'a>(
    path: &'a str,
    ticks: bool,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Crossfader::builder()
        .path(path)
        .ticks(ticks)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn fader<'a>(
    path: &'a str,
    style: FaderStyle,
    label: Option<&'a str>,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Fader::builder()
        .path(path)
        .style(style)
        .maybe_label(label)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn chip<'a>(
    path: &'a str,
    label: &'a str,
    style: ChipStyle,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Chip::builder()
        .path(path)
        .label(label)
        .style(style)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn knob<'a>(
    path: &'a str,
    label: Option<&'a str>,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Knob::builder()
        .path(path)
        .maybe_label(label)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn readout<'a>(
    label: Option<InternId>,
    tone: Tone,
    framed: bool,
    value: Option<&ReadValue<'_>>,
    ui: &'a CompiledUi,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Readout::builder()
        .maybe_label(label.map(|id| ui.resolve(id)))
        .tone(tone)
        .framed(framed)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn segmented<'a>(
    path: &'a str,
    items: &[InternId],
    value: Option<&ReadValue<'_>>,
    ui: &'a CompiledUi,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    Segmented::builder()
        .path(path)
        .items(items.iter().map(|id| ui.resolve(*id)).collect())
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn select<'a>(label: InternId, ui: &'a CompiledUi, skin: &Skin) -> Element<'a, UiEvent> {
    Select::builder()
        .label(ui.resolve(label))
        .skin(skin)
        .build()
        .view()
}

pub(super) fn swatch<'a>(
    role: ColorRole,
    label: InternId,
    ui: &'a CompiledUi,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    Swatch::builder()
        .role(role)
        .label(ui.resolve(label))
        .skin(skin)
        .build()
        .view()
}

pub(super) fn status_dot<'a>(
    label: InternId,
    tone: Tone,
    ui: &'a CompiledUi,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    StatusDot::builder()
        .label(ui.resolve(label))
        .tone(tone)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn cell<'a>(
    label: Option<InternId>,
    highlighted: bool,
    ui: &'a CompiledUi,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    Cell::builder()
        .maybe_label(label.map(|id| ui.resolve(id)))
        .highlighted(highlighted)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn glyph(icon: IconName, style: GlyphStyle, skin: &Skin) -> Element<'static, UiEvent> {
    let vis = style == GlyphStyle::Vis;
    Glyph::builder()
        .icon(render_icon(icon))
        .size(if vis {
            skin.vis.icon_size
        } else {
            skin.nav.header_icon_size
        })
        .color(if vis {
            skin.color(skin.vis.icon_color)
        } else {
            skin.palette.text
        })
        .build()
        .view()
}

pub(super) fn nav_item<'a>(
    path: &'a str,
    label: &'a str,
    icon: IconName,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    NavItem::builder()
        .path(path)
        .label(label)
        .icon(render_icon(icon))
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn tab_large<'a>(
    path: &'a str,
    label: &'a str,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    TabLarge::builder()
        .path(path)
        .label(label)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}
