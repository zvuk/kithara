use std::collections::BTreeMap;

use iced::{
    Background, Element, Length,
    widget::{Column, Row, Space, container, container::Style as ContainerStyle},
};
use num_traits::cast::AsPrimitive;

use crate::{
    atoms::{
        chip::Chip,
        design::{
            cell::Cell, crossfader::Crossfader, segmented::Segmented, select::Select,
            status_dot::StatusDot, swatch::Swatch,
        },
        knob::Knob,
        meter::StereoMeter,
        readout::Readout,
        toggle::{Checkbox, Toggle},
        vu::VerticalVu,
    },
    compile::{CompiledNode, CompiledUi},
    expand::{Binding, ControlSpec, ExpandedNode},
    ids::InternId,
    layout::Axis,
    module::{
        ChromeStyle, DeckSummaryStyle, GlyphStyle, IconName, Tone, TrackColumn, WaveStyle,
        WindowControlsStyle,
    },
    render::{Icon, ReadValue, Reads, Skin, TreeIcon, UiEvent},
    size::{Dim, SizeSpec, control_size},
    skin::ColorRole,
    widgets::{
        ModuleChrome, Widget,
        button::ControlButton,
        deck::{Bpm, DeckSummary, Time},
        fader::Fader,
        global_bar::{Brand, PresetSelector, SettingsButton, Spacer},
        mini_wave::MiniWave,
        nav::{ContextBar, Glyph, NavItem, TabLarge, Tree},
        telemetry::Telemetry,
        text::Text,
        track_list::TrackList,
        vis::Vis,
        wave::zoom_math::DEFAULT_ZOOM,
        window::{TitleBar, WindowControls},
    },
};

pub fn render<'a>(
    node: &CompiledNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    render_compiled(node, ui, reads, skin)
}

fn render_compiled<'a>(
    node: &CompiledNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    let palette = skin.palette;
    match node {
        CompiledNode::Split { axis, children, .. } => match axis {
            Axis::Horizontal => container(
                Row::with_children(children.iter().map(|(weight, child)| {
                    container(render_compiled(child, ui, reads, skin))
                        .width(split_length(child_size(child).w, *weight, skin))
                        .height(Length::Fill)
                        .into()
                }))
                .spacing(skin.layout.grid_gap)
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| {
                ContainerStyle::default().background(Background::Color(palette.line_soft))
            })
            .into(),
            Axis::Vertical => container(
                Column::with_children(children.iter().map(|(weight, child)| {
                    container(render_compiled(child, ui, reads, skin))
                        .width(Length::Fill)
                        .height(split_length(child_size(child).h, *weight, skin))
                        .into()
                }))
                .spacing(skin.layout.grid_gap)
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| {
                ContainerStyle::default().background(Background::Color(palette.line_soft))
            })
            .into(),
        },
        CompiledNode::Module {
            module,
            title,
            chip,
            chrome,
            frame,
            corners,
            footer,
            collapsed,
            root,
            ..
        } => {
            let collapsed = *chrome == ChromeStyle::Full
                && matches!(
                    reads.get(ui.resolve(*collapsed)),
                    Some(ReadValue::Bool(true))
                );
            let footer = footer
                .as_ref()
                .and_then(|binding| resolve(reads, binding, ui))
                .and_then(|value| match value {
                    ReadValue::Text(text) => Some(text.to_owned()),
                    _ => None,
                });
            let content: Element<'a, UiEvent> = if collapsed {
                Space::new().into()
            } else {
                render_node(root, ui, reads, skin)
            };
            ModuleChrome::builder()
                .content(content)
                .maybe_title(title.map(|id| ui.resolve(id)))
                .maybe_chip(chip.map(|id| ui.resolve(id)))
                .style(*chrome)
                .frame(*frame)
                .corners(*corners)
                .maybe_footer(footer)
                .on_toggle(UiEvent::ToggleModule(ui.resolve(*module).to_owned()))
                .collapsed(collapsed)
                .skin(skin)
                .build()
                .view()
        }
    }
}

fn render_node<'a>(
    node: &ExpandedNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    let element = match node {
        ExpandedNode::Row {
            children, gap, pad, ..
        } => container(
            Row::with_children(
                children
                    .iter()
                    .map(|child| render_node(child, ui, reads, skin)),
            )
            .spacing(gap.unwrap_or(skin.layout.grid_gap))
            .width(Length::Fill),
        )
        .padding(pad.unwrap_or(skin.layout.grid_pad))
        .width(Length::Fill)
        .into(),
        ExpandedNode::Column {
            children, gap, pad, ..
        } => container(
            Column::with_children(
                children
                    .iter()
                    .map(|child| render_node(child, ui, reads, skin)),
            )
            .spacing(gap.unwrap_or(skin.layout.grid_gap))
            .width(Length::Fill),
        )
        .padding(pad.unwrap_or(skin.layout.grid_pad))
        .width(Length::Fill)
        .into(),
        ExpandedNode::Slot { children, .. } => container(
            Column::with_children(
                children
                    .iter()
                    .map(|child| render_node(child, ui, reads, skin)),
            )
            .spacing(skin.layout.grid_gap)
            .width(Length::Fill),
        )
        .width(Length::Fill)
        .into(),
        ExpandedNode::Control {
            path, spec, read, ..
        } => render_control(*path, spec, read.as_ref(), ui, reads, skin),
    };
    apply_size(element, effective_size(node, skin))
}

fn render_control<'a>(
    path: InternId,
    spec: &ControlSpec,
    read: Option<&Binding>,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    let value = read.and_then(|binding| resolve(reads, binding, ui));
    let value = value.as_ref();
    let path = ui.resolve(path);
    match spec {
        ControlSpec::DeckSummary { style } => render_deck_summary(*style, value, reads, skin),
        ControlSpec::Brand => Brand::builder().skin(skin).build().view(),
        ControlSpec::Spacer => Spacer::builder().skin(skin).build().view(),
        ControlSpec::PresetSelector => PresetSelector::builder()
            .reads(reads)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::SettingsButton => SettingsButton::builder().skin(skin).build().view(),
        ControlSpec::TitleBar { label } => render_titlebar(*label, ui, skin),
        ControlSpec::WindowControls { style } => render_window_controls(*style, skin),
        ControlSpec::Bpm { placeholder } => Bpm::builder()
            .maybe_placeholder(placeholder.map(|id| ui.resolve(id)))
            .maybe_value(value)
            .reads(reads)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Time => render_time(value, reads, skin),
        ControlSpec::Text { style, label } => Text::builder()
            .style(*style)
            .maybe_value(value)
            .maybe_label(label.as_ref().map(|id| ui.resolve(*id)))
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Glyph { icon, style } => render_glyph(*icon, *style, skin),
        ControlSpec::NavItem { label, icon } => {
            render_nav_item(path, ui.resolve(*label), *icon, value, skin)
        }
        ControlSpec::TabLarge { label } => render_tab_large(path, ui.resolve(*label), value, skin),
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
        ControlSpec::Scalar { format } => Telemetry::builder()
            .format(*format)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Crossfader => render_crossfader(path, value, skin),
        ControlSpec::Fader { style } => Fader::builder()
            .path(path)
            .style(*style)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
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
        ControlSpec::Segmented { items } => render_segmented(path, items, value, ui, skin),
        ControlSpec::Select { label } => render_select(*label, ui, skin),
        ControlSpec::StatusDot { label, tone } => render_status_dot(*label, *tone, ui, skin),
        ControlSpec::Swatch { role, label } => render_swatch(*role, *label, ui, skin),
        ControlSpec::Cell { label, highlighted } => render_cell(*label, *highlighted, ui, skin),
        ControlSpec::Readout {
            label,
            tone,
            framed,
        } => Readout::builder()
            .maybe_label(label.map(|id| ui.resolve(id)))
            .tone(*tone)
            .framed(*framed)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Chip { label, style } => Chip::builder()
            .path(path)
            .label(ui.resolve(*label))
            .style(*style)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Knob => Knob::builder()
            .path(path)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::VuStereo => StereoMeter::builder()
            .path(path)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::VuVertical => VerticalVu::builder()
            .path(path)
            .maybe_value(value)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Vis => render_vis(value, reads),
        ControlSpec::Wave { style, badge, zoom } => render_wave(
            path,
            *style,
            badge.map(|id| ui.resolve(id)),
            wave_zoom(zoom.as_ref(), reads, ui),
            value,
            reads,
            skin,
        ),
        ControlSpec::TrackList {
            columns,
            columns_state,
        } => render_track_list(
            path,
            columns,
            columns_state.as_ref(),
            value,
            ui,
            reads,
            skin,
        ),
        ControlSpec::Tree { query } => render_tree(path, query.as_ref(), value, ui, reads, skin),
        ControlSpec::ContextBar { scope_items, scope } => {
            render_context_bar(path, scope_items, scope.as_ref(), value, ui, reads, skin)
        }
    }
}

fn render_crossfader<'a>(
    path: &'a str,
    value: Option<&ReadValue<'_>>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Crossfader::builder()
        .path(path)
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

fn render_time<'a>(
    value: Option<&ReadValue<'_>>,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    Time::builder()
        .maybe_value(value)
        .reads(reads)
        .skin(skin)
        .build()
        .view()
}

fn render_vis<'a>(value: Option<&ReadValue<'_>>, reads: &dyn Reads) -> Element<'a, UiEvent> {
    Vis::builder()
        .maybe_preset(value)
        .reads(reads)
        .build()
        .view()
}

fn render_deck_summary<'a>(
    style: DeckSummaryStyle,
    value: Option<&ReadValue<'_>>,
    reads: &dyn Reads,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    DeckSummary::builder()
        .style(style)
        .maybe_value(value)
        .reads(reads)
        .skin(skin)
        .build()
        .view()
}

fn render_titlebar<'a>(label: InternId, ui: &'a CompiledUi, skin: &Skin) -> Element<'a, UiEvent> {
    TitleBar::builder()
        .label(ui.resolve(label))
        .skin(skin)
        .build()
        .view()
}

fn render_window_controls(style: WindowControlsStyle, skin: &Skin) -> Element<'static, UiEvent> {
    WindowControls::builder()
        .style(style)
        .skin(skin)
        .build()
        .view()
}

fn render_track_list<'a>(
    path: &'a str,
    columns: &[TrackColumn],
    columns_state: Option<&Binding>,
    value: Option<&ReadValue<'_>>,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    TrackList::builder()
        .path(path)
        .columns(columns)
        .maybe_columns_state(columns_state.map(|binding| binding_id(binding, ui)))
        .maybe_value(value)
        .reads(reads)
        .skin(skin)
        .build()
        .view()
}

fn render_tree<'a>(
    path: &'a str,
    query: Option<&Binding>,
    value: Option<&ReadValue<'_>>,
    ui: &CompiledUi,
    reads: &dyn Reads,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let query = query
        .and_then(|binding| resolve(reads, binding, ui))
        .and_then(|value| match value {
            ReadValue::Text(query) => Some(query),
            _ => None,
        })
        .unwrap_or_default();
    Tree::builder()
        .path(path)
        .query(query)
        .maybe_value(value)
        .icon(render_tree_icon)
        .skin(skin)
        .build()
        .view()
}

fn render_wave<'a>(
    path: &'a str,
    style: WaveStyle,
    badge: Option<&'a str>,
    zoom: f32,
    value: Option<&ReadValue<'_>>,
    reads: &dyn Reads,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    MiniWave::builder()
        .path(path)
        .style(style)
        .zoom(zoom)
        .maybe_badge(badge)
        .maybe_value(value)
        .reads(reads)
        .skin(skin)
        .build()
        .view()
}

fn wave_zoom(zoom: Option<&Binding>, reads: &dyn Reads, ui: &CompiledUi) -> f32 {
    zoom.and_then(|binding| resolve(reads, binding, ui))
        .and_then(|value| match value {
            ReadValue::Scalar(value) => Some(value.as_()),
            _ => None,
        })
        .unwrap_or(DEFAULT_ZOOM)
}

fn render_context_bar<'a>(
    path: &'a str,
    scope_items: &[InternId],
    scope: Option<&Binding>,
    value: Option<&ReadValue<'_>>,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let scope_value = scope.and_then(|binding| resolve(reads, binding, ui));
    ContextBar::builder()
        .path(path)
        .scope_items(scope_items.iter().map(|id| ui.resolve(*id)).collect())
        .maybe_scope_value(scope_value.as_ref())
        .maybe_value(value)
        .skin(skin)
        .build()
        .view()
}

fn render_segmented<'a>(
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

fn render_select<'a>(label: InternId, ui: &'a CompiledUi, skin: &Skin) -> Element<'a, UiEvent> {
    Select::builder()
        .label(ui.resolve(label))
        .skin(skin)
        .build()
        .view()
}

fn render_swatch<'a>(
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

fn render_status_dot<'a>(
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

fn render_cell<'a>(
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

fn render_glyph(icon: IconName, style: GlyphStyle, skin: &Skin) -> Element<'static, UiEvent> {
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

fn render_nav_item<'a>(
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

fn render_tab_large<'a>(
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

fn resolve<'a>(reads: &'a dyn Reads, binding: &Binding, ui: &CompiledUi) -> Option<ReadValue<'a>> {
    match binding {
        Binding::Telemetry { id, with } if with.is_empty() || deck_is_a(with, ui) => {
            reads.get(ui.resolve(*id))
        }
        Binding::Parameter { id, .. } | Binding::Model { id, .. } => reads.get(ui.resolve(*id)),
        _ => None,
    }
}

fn binding_id<'a>(binding: &Binding, ui: &'a CompiledUi) -> &'a str {
    let id = match binding {
        Binding::Command { id, .. }
        | Binding::Parameter { id, .. }
        | Binding::Telemetry { id, .. }
        | Binding::Model { id, .. } => *id,
    };
    ui.resolve(id)
}

fn deck_is_a(scope: &BTreeMap<InternId, InternId>, ui: &CompiledUi) -> bool {
    scope
        .iter()
        .any(|(key, value)| ui.resolve(*key) == "deck" && ui.resolve(*value) == "a")
}

fn effective_size(node: &ExpandedNode, skin: &Skin) -> Option<SizeSpec> {
    let declared = match node {
        ExpandedNode::Row { size, .. }
        | ExpandedNode::Column { size, .. }
        | ExpandedNode::Slot { size, .. }
        | ExpandedNode::Control { size, .. } => *size,
    };
    declared.or_else(|| match node {
        ExpandedNode::Control {
            spec: ControlSpec::TabLarge { .. },
            ..
        } => None,
        ExpandedNode::Control { spec, .. } => Some(control_size(spec, skin.document())),
        _ => None,
    })
}

fn render_icon(icon: IconName) -> Icon {
    match icon {
        IconName::ChevronUp => Icon::ChevronUp,
        IconName::Disc => Icon::Disc,
        IconName::Faders => Icon::Faders,
        IconName::FastForward => Icon::FastForward,
        IconName::Gear => Icon::Gear,
        IconName::Headphones => Icon::Headphones,
        IconName::Maximize => Icon::Maximize,
        IconName::Menu => Icon::Menu,
        IconName::Play => Icon::Play,
        IconName::PlayReverse => Icon::PlayReverse,
        IconName::Playlist => Icon::Playlist,
        IconName::Rewind => Icon::Rewind,
        IconName::SpeakerHigh => Icon::SpeakerHigh,
        IconName::Waveform => Icon::Waveform,
        IconName::X => Icon::X,
        IconName::ZoomIn => Icon::ZoomIn,
        IconName::ZoomOut => Icon::ZoomOut,
    }
}

fn render_tree_icon(icon: TreeIcon) -> Icon {
    match icon {
        TreeIcon::Collection => Icon::Collection,
        TreeIcon::Playlist => Icon::Playlist,
        TreeIcon::Folder => Icon::Folder,
        TreeIcon::Plus => Icon::Plus,
        TreeIcon::Zvuk => Icon::Zvuk,
        TreeIcon::Search => Icon::Search,
        TreeIcon::Charts => Icon::Charts,
        TreeIcon::Monitor => Icon::Monitor,
        TreeIcon::Home => Icon::Home,
        TreeIcon::Usb => Icon::Usb,
        TreeIcon::Instrument => Icon::Instrument,
        TreeIcon::Waveform => Icon::Waveform,
        TreeIcon::Clock => Icon::Clock,
    }
}

fn apply_size<'a>(element: Element<'a, UiEvent>, size: Option<SizeSpec>) -> Element<'a, UiEvent> {
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

fn child_size(node: &CompiledNode) -> SizeSpec {
    match node {
        CompiledNode::Split { size, .. } | CompiledNode::Module { size, .. } => *size,
    }
}

fn split_length(dim: Dim, weight: f32, skin: &Skin) -> Length {
    match dim {
        Dim::Fixed(value) => Length::Fixed(value),
        _ => Length::FillPortion(fill_portion(weight, skin)),
    }
}

fn fill_portion(weight: f32, skin: &Skin) -> u16 {
    let scaled = (weight * skin.layout.fill_weight_scale)
        .round()
        .max(skin.layout.fill_weight_min)
        .min(f32::from(u16::MAX));
    scaled.as_()
}

#[cfg(test)]
mod tests {
    use iced::{Size, widget::Space};
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn fixed_size_spec_sets_both_element_axes() {
        let element: Element<'static, UiEvent> = Space::new().into();
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
        let element: Element<'static, UiEvent> = Space::new()
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
