use iced::alignment::Horizontal;

use super::{
    atom::{
        cell, checkbox, chip, crossfader, fader, glyph, knob, meter, nav_item, portal_map, range,
        readout, segmented, select, status_dot, swatch, tab_large, toggle, vu_stereo, vu_vertical,
    },
    geometry::Rendered,
    icon::render_icon,
    panel::{browser_tree, context_bar, deck_summary, time, track_list, vis},
    read::{read_flag, read_scope, resolve, wave_zoom},
    window::{titlebar, window_controls},
};
use crate::{
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
        telemetry::Telemetry,
        text::Text,
        wave::mini::MiniWave,
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
            color,
            active_color,
            active,
            ..
        } => Text::builder()
            .style(*style)
            .maybe_value(value)
            .maybe_label(label.map(|id| ui.resolve(id)))
            .maybe_color(*color)
            .maybe_active_color(*active_color)
            .active(read_flag(active.as_ref(), reads, ui))
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Glyph {
            icon,
            active_icon,
            style,
            color,
            active_color,
            active,
        } => glyph(
            *icon,
            *active_icon,
            *style,
            *color,
            *active_color,
            read_flag(active.as_ref(), reads, ui),
            skin,
        ),
        ControlSpec::NavItem { label, icon } => {
            nav_item(path, ui.resolve(*label), *icon, value, skin)
        }
        ControlSpec::TabLarge { label } => tab_large(path, ui.resolve(*label), value, skin),
        ControlSpec::Button {
            label,
            icon,
            active_label,
            style,
            frame,
        } => ControlButton::builder()
            .path(path)
            .label(ui.resolve(*label))
            .maybe_icon(icon.map(render_icon))
            .maybe_active_label(active_label.map(|id| ui.resolve(id)))
            .style(*style)
            .maybe_frame(*frame)
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
        ControlSpec::Toggle => toggle(path, value, skin),
        ControlSpec::Checkbox => checkbox(path, value, skin),
        ControlSpec::Segmented { items } => segmented(path, items, value, ui, skin),
        ControlSpec::Select { label } => select(*label, ui, skin),
        ControlSpec::StatusDot {
            label,
            dot_size,
            tone,
            active_tone,
            active,
        } => {
            let on = read_flag(active.as_ref(), reads, ui);
            status_dot(*label, *dot_size, *tone, *active_tone, on, ui, skin)
        }
        ControlSpec::Swatch { role, label } => swatch(*role, *label, ui, skin),
        ControlSpec::Cell { label, highlighted } => cell(*label, *highlighted, ui, skin),
        ControlSpec::Readout {
            label,
            tone,
            framed,
        } => readout(*label, *tone, *framed, value, ui, skin),
        ControlSpec::Chip { label, style } => chip(path, ui.resolve(*label), *style, value, skin),
        ControlSpec::Knob { label } => knob(path, label.map(|id| ui.resolve(id)), value, skin),
        ControlSpec::VuStereo => vu_stereo(path, value, skin),
        ControlSpec::VuVertical { ticks } => vu_vertical(path, *ticks, value, skin),
        ControlSpec::Vis => vis(value, reads),
        ControlSpec::PortalMap => portal_map(value, skin),
        ControlSpec::Range => range(path, value, skin),
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
        ControlSpec::Meter => meter(value, skin),
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
    Rendered::new(element, alignment(spec))
}

fn alignment(spec: &ControlSpec) -> Horizontal {
    let ControlSpec::Text { align, .. } = spec else {
        return Horizontal::Left;
    };
    match align {
        TextAlign::Start => Horizontal::Left,
        TextAlign::Center => Horizontal::Center,
        TextAlign::End => Horizontal::Right,
    }
}
