use iced::{Color, Element};

use super::{geometry::active_tone, icon::render_icon};
use crate::{
    atoms::{
        chip::Chip,
        design::{
            cell::Cell, crossfader::Crossfader, meter::Meter, segmented::Segmented, select::Select,
            status_dot::StatusDot, swatch::Swatch,
        },
        knob::Knob,
        meter::StereoMeter,
        readout::Readout,
        toggle::{Checkbox, Toggle},
        vu::VerticalVu,
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

pub(super) fn toggle<'a>(
    path: &'a str,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Toggle::builder()
        .path(path)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn checkbox<'a>(
    path: &'a str,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Checkbox::builder()
        .path(path)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn vu_stereo<'a>(
    path: &'a str,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    StereoMeter::builder()
        .path(path)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn vu_vertical<'a>(
    path: &'a str,
    ticks: bool,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    VerticalVu::builder()
        .path(path)
        .ticks(ticks)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

pub(super) fn meter<'a>(value: Option<&ReadValue<'_>>, skin: &'a Skin) -> Element<'a, UiEvent> {
    Meter::builder()
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

pub(super) fn glyph(
    icon: IconName,
    active_icon: Option<IconName>,
    style: GlyphStyle,
    color: Option<ColorRole>,
    active_color: Option<ColorRole>,
    active: bool,
    skin: &Skin,
) -> Element<'static, UiEvent> {
    let icon = active.then_some(active_icon).flatten().unwrap_or(icon);
    let tone = glyph_tone(color, active_color, active, skin);
    Glyph::builder()
        .icon(render_icon(icon))
        .size(glyph_size(style, skin))
        .color(tone.unwrap_or_else(|| glyph_base(style, skin)))
        .build()
        .view()
}

/// Icon side, in logical pixels, for the style the node declares.
fn glyph_size(style: GlyphStyle, skin: &Skin) -> f32 {
    match style {
        GlyphStyle::Default => skin.nav.header_icon_size,
        GlyphStyle::Vis => skin.vis.icon_size,
        GlyphStyle::Menu => skin.menu.icon_size,
        GlyphStyle::MenuBurger => skin.menu.burger_icon_size,
        GlyphStyle::MenuSmall => skin.menu.small_icon_size,
        GlyphStyle::MenuCell => skin.menu.cell_icon_size,
    }
}

/// Tone a style carries when the node names no colour of its own.
fn glyph_base(style: GlyphStyle, skin: &Skin) -> Color {
    match style {
        GlyphStyle::Vis => skin.color(skin.vis.icon_color),
        GlyphStyle::Default
        | GlyphStyle::Menu
        | GlyphStyle::MenuBurger
        | GlyphStyle::MenuSmall
        | GlyphStyle::MenuCell => skin.palette.text,
    }
}

/// Joins the colour pair a glyph declares to the palette. `None` leaves the
/// tone to the style.
fn glyph_tone(
    color: Option<ColorRole>,
    active_color: Option<ColorRole>,
    active: bool,
    skin: &Skin,
) -> Option<Color> {
    active_tone(color, active_color, active).map(|role| skin.color(role))
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::builtin;

    #[kithara::test]
    fn every_glyph_style_takes_its_own_skin_icon_size() {
        let skin = builtin::skin();

        for (style, size) in [
            (GlyphStyle::Default, skin.nav.header_icon_size),
            (GlyphStyle::Vis, skin.vis.icon_size),
            (GlyphStyle::Menu, skin.menu.icon_size),
            (GlyphStyle::MenuBurger, skin.menu.burger_icon_size),
            (GlyphStyle::MenuSmall, skin.menu.small_icon_size),
            (GlyphStyle::MenuCell, skin.menu.cell_icon_size),
        ] {
            assert_eq!(glyph_size(style, skin), size, "{style:?}");
        }
    }

    #[kithara::test]
    fn a_declared_glyph_pair_switches_on_the_active_flag() {
        let skin = builtin::skin();
        let tone = |active| {
            glyph_tone(
                Some(ColorRole::Muted),
                Some(ColorRole::Danger),
                active,
                skin,
            )
        };

        assert_eq!(tone(false), Some(skin.color(ColorRole::Muted)));
        assert_eq!(tone(true), Some(skin.color(ColorRole::Danger)));
    }

    #[kithara::test]
    fn an_active_glyph_naming_no_active_colour_keeps_its_base() {
        let skin = builtin::skin();
        let tone = glyph_tone(Some(ColorRole::Accent), None, true, skin);

        assert_eq!(tone, Some(skin.color(ColorRole::Accent)));
    }

    #[kithara::test]
    fn a_glyph_naming_no_colour_leaves_the_tone_to_its_style() {
        let skin = builtin::skin();

        assert_eq!(glyph_tone(None, None, false, skin), None);
        assert_eq!(glyph_tone(None, None, true, skin), None);
        assert_eq!(
            glyph_base(GlyphStyle::Vis, skin),
            skin.color(skin.vis.icon_color)
        );

        for style in [
            GlyphStyle::Default,
            GlyphStyle::Menu,
            GlyphStyle::MenuBurger,
            GlyphStyle::MenuSmall,
            GlyphStyle::MenuCell,
        ] {
            assert_eq!(glyph_base(style, skin), skin.palette.text, "{style:?}");
        }
    }
}
