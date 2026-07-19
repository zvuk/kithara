use std::collections::BTreeMap;

use iced::{
    Background, Element, Length,
    widget::{Column, Row, container, container::Style as ContainerStyle},
};
use num_traits::cast::AsPrimitive;

use crate::{
    atoms::{chip, knob, meter, readout, toggle, vu},
    compile::{CompiledNode, CompiledUi},
    expand::{Binding, ControlSpec, ExpandedNode},
    ids::InternId,
    layout::Axis,
    render::{ReadValue, Reads, RenderPalette, UiEvent},
    size::{Dim, SizeSpec, control_size},
    widgets::{
        button, deck, fader, global_bar, mini_wave, module_chrome, telemetry, text, track_list,
    },
};

struct Consts;

impl Consts {
    const FILL_WEIGHT_SCALE: f32 = 100.0;
    const GRID_GAP: f32 = 1.0;
}

pub fn render<'a>(
    node: &CompiledNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    render_compiled(node, ui, reads, palette)
}

fn render_compiled<'a>(
    node: &CompiledNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    match node {
        CompiledNode::Split { axis, children, .. } => match axis {
            Axis::Horizontal => container(
                Row::with_children(children.iter().map(|(weight, child)| {
                    container(render_compiled(child, ui, reads, palette))
                        .width(split_length(child_size(child).w, *weight))
                        .height(Length::Fill)
                        .into()
                }))
                .spacing(Consts::GRID_GAP)
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
                    container(render_compiled(child, ui, reads, palette))
                        .width(Length::Fill)
                        .height(split_length(child_size(child).h, *weight))
                        .into()
                }))
                .spacing(Consts::GRID_GAP)
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
        CompiledNode::Module { root, .. } => {
            module_chrome(render_node(root, ui, reads, palette), palette)
        }
    }
}

fn render_node<'a>(
    node: &ExpandedNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let element = match node {
        ExpandedNode::Row {
            children, gap, pad, ..
        } => container(
            Row::with_children(
                children
                    .iter()
                    .map(|child| render_node(child, ui, reads, palette)),
            )
            .spacing(gap.unwrap_or(Consts::GRID_GAP))
            .width(Length::Fill),
        )
        .padding(pad.unwrap_or(0.0))
        .width(Length::Fill)
        .into(),
        ExpandedNode::Column {
            children, gap, pad, ..
        } => container(
            Column::with_children(
                children
                    .iter()
                    .map(|child| render_node(child, ui, reads, palette)),
            )
            .spacing(gap.unwrap_or(Consts::GRID_GAP))
            .width(Length::Fill),
        )
        .padding(pad.unwrap_or(0.0))
        .width(Length::Fill)
        .into(),
        ExpandedNode::Slot { children, .. } => container(
            Column::with_children(
                children
                    .iter()
                    .map(|child| render_node(child, ui, reads, palette)),
            )
            .spacing(Consts::GRID_GAP)
            .width(Length::Fill),
        )
        .width(Length::Fill)
        .into(),
        ExpandedNode::Control {
            path, spec, read, ..
        } => render_control(*path, spec, read.as_ref(), ui, reads, palette),
    };
    apply_size(element, effective_size(node))
}

fn render_control<'a>(
    path: InternId,
    spec: &ControlSpec,
    read: Option<&Binding>,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let value = read.and_then(|binding| resolve(reads, binding, ui));
    let path = ui.resolve(path);
    match spec {
        ControlSpec::DeckHeader { badge } => deck::header(
            badge.map(|id| ui.resolve(id)),
            value.as_ref(),
            reads,
            palette,
        ),
        ControlSpec::DeckSummary { style } => deck::summary(*style, value.as_ref(), reads, palette),
        ControlSpec::Brand => global_bar::brand(palette),
        ControlSpec::Spacer => global_bar::spacer(palette),
        ControlSpec::PresetSelector => global_bar::preset_selector(reads, palette),
        ControlSpec::SettingsButton => global_bar::settings_button(palette),
        ControlSpec::Bpm { placeholder } => deck::bpm(
            placeholder.map(|id| ui.resolve(id)),
            value.as_ref(),
            reads,
            palette,
        ),
        ControlSpec::Time => deck::time(value.as_ref(), reads, palette),
        ControlSpec::Text { style } => text::view(*style, value.as_ref(), palette),
        ControlSpec::Button {
            label,
            active_label,
            style,
        } => button::view(
            path,
            ui.resolve(*label),
            active_label.map(|id| ui.resolve(id)),
            *style,
            value.as_ref(),
            palette,
        ),
        ControlSpec::Scalar { format } => telemetry::view(*format, value.as_ref(), palette),
        ControlSpec::Fader { style } => fader::view(path, *style, value.as_ref(), palette),
        ControlSpec::Toggle => toggle::toggle(path, value.as_ref(), palette),
        ControlSpec::Checkbox => toggle::checkbox(path, value.as_ref(), palette),
        ControlSpec::Readout {
            label,
            tone,
            framed,
        } => readout::view(
            label.map(|id| ui.resolve(id)),
            *tone,
            *framed,
            value.as_ref(),
            palette,
        ),
        ControlSpec::Chip { label } => {
            chip::view(path, ui.resolve(*label), value.as_ref(), palette)
        }
        ControlSpec::Knob => knob::view(path, value.as_ref(), palette),
        ControlSpec::VuStereo => meter::view(path, value.as_ref(), palette),
        ControlSpec::VuVertical => vu::view(path, value.as_ref(), palette),
        ControlSpec::Wave { style } => {
            mini_wave::view(path, *style, value.as_ref(), reads, palette)
        }
        ControlSpec::TrackList => track_list::view(path, value.as_ref(), reads, palette),
    }
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

fn effective_size(node: &ExpandedNode) -> Option<SizeSpec> {
    let declared = match node {
        ExpandedNode::Row { size, .. }
        | ExpandedNode::Column { size, .. }
        | ExpandedNode::Slot { size, .. }
        | ExpandedNode::Control { size, .. } => *size,
    };
    declared.or_else(|| match node {
        ExpandedNode::Control { spec, .. } => Some(control_size(spec)),
        _ => None,
    })
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

fn split_length(dim: Dim, weight: f32) -> Length {
    match dim {
        Dim::Fixed(value) => Length::Fixed(value),
        _ => Length::FillPortion(fill_portion(weight)),
    }
}

fn fill_portion(weight: f32) -> u16 {
    let scaled = (weight * Consts::FILL_WEIGHT_SCALE)
        .round()
        .max(1.0)
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
