use iced::alignment::Horizontal;

use super::{
    atom::{
        cell, chip, crossfader, fader, glyph, knob, nav_item, readout, segmented, select,
        status_dot, swatch, tab_large,
    },
    geometry::Rendered,
    icon::render_icon,
    panel::{browser_tree, context_bar, deck_summary, time, track_list, vis},
    read::{read_flag, read_scope, resolve, wave_zoom},
    window::{titlebar, window_controls},
};
use crate::{
    atoms::{
        design::meter::Meter,
        meter::StereoMeter,
        toggle::{Checkbox, Toggle},
        vu::VerticalVu,
    },
    compile::CompiledUi,
    expand::{Binding, ControlSpec},
    ids::InternId,
    module::TextAlign,
    render::{Reads, Skin},
    widgets::{
        Widget,
        button::ControlButton,
        deck::Bpm,
        global_bar::{Brand, Divider, PresetSelector, SettingsButton, Spacer},
        mini_wave::MiniWave,
        telemetry::Telemetry,
        text::Text,
        window::WindowSurface,
    },
};

pub(super) fn render_control<'a>(
    path: InternId,
    spec: &ControlSpec,
    read: Option<&Binding>,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Rendered<'a> {
    let value = read.and_then(|binding| resolve(reads, binding, ui));
    let value = value.as_ref();
    let path = ui.resolve(path);
    let scope = read_scope(read, ui);
    let mut align = Horizontal::Left;
    let element = match spec {
        ControlSpec::DeckSummary { style } => deck_summary(*style, value, scope, reads, skin),
        ControlSpec::Brand => Brand::builder().skin(skin).build().view(),
        ControlSpec::Spacer => Spacer::builder().skin(skin).build().view(),
        ControlSpec::Divider => Divider::builder().skin(skin).build().view(),
        ControlSpec::PresetSelector => PresetSelector::builder()
            .reads(reads)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::SettingsButton => SettingsButton::builder().skin(skin).build().view(),
        ControlSpec::WindowDrag => WindowSurface::drag().view(),
        ControlSpec::TitleBar { label } => titlebar(*label, ui, skin),
        ControlSpec::WindowControls { style } => window_controls(*style, skin),
        ControlSpec::Bpm { placeholder } => Bpm::builder()
            .maybe_placeholder(placeholder.map(|id| ui.resolve(id)))
            .maybe_value(value)
            .scope(scope)
            .reads(reads)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Time => time(value, scope, reads, skin),
        ControlSpec::Text {
            style,
            label,
            active,
            align: declared,
        } => {
            align = horizontal(*declared);
            Text::builder()
                .style(*style)
                .maybe_value(value)
                .maybe_label(label.map(|id| ui.resolve(id)))
                .active(read_flag(active.as_ref(), reads, ui))
                .skin(skin)
                .build()
                .view()
        }
        ControlSpec::Glyph { icon, style } => glyph(*icon, *style, skin),
        ControlSpec::NavItem { label, icon } => {
            nav_item(path, ui.resolve(*label), *icon, value, skin)
        }
        ControlSpec::TabLarge { label } => tab_large(path, ui.resolve(*label), value, skin),
        ControlSpec::Button {
            label,
            icon,
            active_label,
            style,
        } => ControlButton::builder()
            .path(path)
            .label(ui.resolve(*label))
            .maybe_icon(icon.map(render_icon))
            .maybe_active_label(active_label.map(|id| ui.resolve(id)))
            .style(*style)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Scalar { format, framed } => Telemetry::builder()
            .format(*format)
            .framed(*framed)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Crossfader { ticks } => crossfader(path, *ticks, value, skin),
        ControlSpec::Fader { style, label } => {
            fader(path, *style, label.map(|id| ui.resolve(id)), value, skin)
        }
        ControlSpec::Toggle => Toggle::builder()
            .path(path)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Checkbox => Checkbox::builder()
            .path(path)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Segmented { items } => segmented(path, items, value, ui, skin),
        ControlSpec::Select { label } => select(*label, ui, skin),
        ControlSpec::StatusDot { label, tone } => status_dot(*label, *tone, ui, skin),
        ControlSpec::Swatch { role, label } => swatch(*role, *label, ui, skin),
        ControlSpec::Cell { label, highlighted } => cell(*label, *highlighted, ui, skin),
        ControlSpec::Readout {
            label,
            tone,
            framed,
        } => readout(*label, *tone, *framed, value, ui, skin),
        ControlSpec::Chip { label, style } => chip(path, ui.resolve(*label), *style, value, skin),
        ControlSpec::Knob { label } => knob(path, label.map(|id| ui.resolve(id)), value, skin),
        ControlSpec::VuStereo => StereoMeter::builder()
            .path(path)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::VuVertical { ticks } => VerticalVu::builder()
            .path(path)
            .ticks(*ticks)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Vis => vis(value, reads),
        ControlSpec::Wave { style, badge, zoom } => MiniWave::builder()
            .path(path)
            .style(*style)
            .zoom(wave_zoom(zoom.as_ref(), reads, ui))
            .maybe_badge(badge.map(|id| ui.resolve(id)))
            .maybe_value(value)
            .scope(scope)
            .reads(reads)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Meter => Meter::builder()
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::TrackList {
            columns,
            columns_state,
        } => track_list(
            path,
            columns,
            columns_state.as_ref(),
            value,
            ui,
            reads,
            skin,
        ),
        ControlSpec::Tree { query } => browser_tree(path, query.as_ref(), value, ui, reads, skin),
        ControlSpec::ContextBar { scope_items, scope } => {
            context_bar(path, scope_items, scope.as_ref(), value, ui, reads, skin)
        }
    };
    Rendered::new(element, align)
}

fn horizontal(align: TextAlign) -> Horizontal {
    match align {
        TextAlign::Start => Horizontal::Left,
        TextAlign::Center => Horizontal::Center,
        TextAlign::End => Horizontal::Right,
    }
}
