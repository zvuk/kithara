use std::collections::BTreeMap;

use iced::{
    Background, Element, Length,
    widget::{Column, Row, container, container::Style as ContainerStyle},
};
use num_traits::cast::AsPrimitive;

use crate::{
    atoms::{
        chip::Chip,
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
    module::IconName,
    render::{Icon, ReadValue, Reads, Skin, UiEvent},
    size::{Dim, SizeSpec, control_size},
    widgets::{
        ModuleChrome, Widget,
        button::ControlButton,
        deck::{Bpm, DeckHeader, DeckSummary, Time},
        fader::Fader,
        global_bar::{Brand, PresetSelector, SettingsButton, Spacer},
        mini_wave::MiniWave,
        nav::{Glyph, NavItem, TabLarge},
        telemetry::Telemetry,
        text::Text,
        track_list::TrackList,
    },
};

pub fn render<'a>(
    node: &CompiledNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    render_compiled(node, ui, reads, skin)
}

fn render_compiled<'a>(
    node: &CompiledNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &Skin,
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
        CompiledNode::Module { root, .. } => ModuleChrome::builder()
            .content(render_node(root, ui, reads, skin))
            .skin(skin)
            .build()
            .view(),
    }
}

fn render_node<'a>(
    node: &ExpandedNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &Skin,
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
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let value = read.and_then(|binding| resolve(reads, binding, ui));
    let path = ui.resolve(path);
    match spec {
        ControlSpec::DeckHeader { badge } => DeckHeader::builder()
            .maybe_badge(badge.map(|id| ui.resolve(id)))
            .maybe_value(value.as_ref())
            .reads(reads)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::DeckSummary { style } => DeckSummary::builder()
            .style(*style)
            .maybe_value(value.as_ref())
            .reads(reads)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Brand => Brand::builder().skin(skin).build().view(),
        ControlSpec::Spacer => Spacer::builder().skin(skin).build().view(),
        ControlSpec::PresetSelector => PresetSelector::builder()
            .reads(reads)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::SettingsButton => SettingsButton::builder().skin(skin).build().view(),
        ControlSpec::Bpm { placeholder } => Bpm::builder()
            .maybe_placeholder(placeholder.map(|id| ui.resolve(id)))
            .maybe_value(value.as_ref())
            .reads(reads)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Time => Time::builder()
            .maybe_value(value.as_ref())
            .reads(reads)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Text { style, label } => Text::builder()
            .style(*style)
            .maybe_value(value.as_ref())
            .maybe_label(label.as_ref().map(|id| ui.resolve(*id)))
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Glyph { icon } => render_glyph(*icon, skin),
        ControlSpec::NavItem { label, icon } => {
            render_nav_item(path, ui.resolve(*label), *icon, value.as_ref(), skin)
        }
        ControlSpec::TabLarge { label } => {
            render_tab_large(path, ui.resolve(*label), value.as_ref(), skin)
        }
        ControlSpec::Button {
            label,
            active_label,
            style,
        } => ControlButton::builder()
            .path(path)
            .label(ui.resolve(*label))
            .maybe_active_label(active_label.map(|id| ui.resolve(id)))
            .style(*style)
            .maybe_value(value.as_ref())
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Scalar { format } => Telemetry::builder()
            .format(*format)
            .maybe_value(value.as_ref())
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Fader { style } => Fader::builder()
            .path(path)
            .style(*style)
            .maybe_value(value.as_ref())
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Toggle => Toggle::builder()
            .path(path)
            .maybe_value(value.as_ref())
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Checkbox => Checkbox::builder()
            .path(path)
            .maybe_value(value.as_ref())
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Readout {
            label,
            tone,
            framed,
        } => Readout::builder()
            .maybe_label(label.map(|id| ui.resolve(id)))
            .tone(*tone)
            .framed(*framed)
            .maybe_value(value.as_ref())
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Chip { label } => Chip::builder()
            .path(path)
            .label(ui.resolve(*label))
            .maybe_value(value.as_ref())
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Knob => Knob::builder()
            .path(path)
            .maybe_value(value.as_ref())
            .skin(skin)
            .build()
            .view(),
        ControlSpec::VuStereo => StereoMeter::builder()
            .path(path)
            .maybe_value(value.as_ref())
            .skin(skin)
            .build()
            .view(),
        ControlSpec::VuVertical => VerticalVu::builder()
            .path(path)
            .maybe_value(value.as_ref())
            .skin(skin)
            .build()
            .view(),
        ControlSpec::Wave { style } => MiniWave::builder()
            .path(path)
            .style(*style)
            .maybe_value(value.as_ref())
            .reads(reads)
            .skin(skin)
            .build()
            .view(),
        ControlSpec::TrackList => TrackList::builder()
            .path(path)
            .maybe_value(value.as_ref())
            .reads(reads)
            .skin(skin)
            .build()
            .view(),
    }
}

fn render_glyph(icon: IconName, skin: &Skin) -> Element<'static, UiEvent> {
    Glyph::builder()
        .icon(render_icon(icon))
        .skin(skin)
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
        Binding::Telemetry { id, with } if deck_is_a(with, ui) => reads.get(ui.resolve(*id)),
        Binding::Parameter { id, .. } | Binding::Model { id, .. } => reads.get(ui.resolve(*id)),
        _ => None,
    }
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
        IconName::Disc => Icon::Disc,
        IconName::Faders => Icon::Faders,
        IconName::Gear => Icon::Gear,
        IconName::Play => Icon::Play,
        IconName::Playlist => Icon::Playlist,
        IconName::SpeakerHigh => Icon::SpeakerHigh,
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
