use std::{collections::BTreeMap, sync::LazyLock};

use iced::{
    Background, Element, Length,
    widget::{Column, Row, Space, container, container::Style as ContainerStyle},
};
use num_traits::cast::AsPrimitive;

use crate::{
    atoms::{chip, knob, meter, readout, toggle, vu},
    compile::{CompiledNode, CompiledUi},
    expand::{Binding, ExpandedNode},
    ids::{ControlKind, InternId},
    layout::Axis,
    module::PropValue,
    registry::{ControlCatalog, ControlKindDesc},
    render::{ReadValue, Reads, RenderPalette, UiEvent},
    size::{Dim, SizeSpec},
    widgets::{
        button, deck, fader, global_bar, mini_wave, module_chrome, telemetry, text, track_list,
    },
};

struct Consts;

impl Consts {
    const FILL_WEIGHT_SCALE: f32 = 100.0;
    const GRID_GAP: f32 = 1.0;
}

#[derive(Default)]
struct Catalog {
    kinds: BTreeMap<ControlKind, ControlKindDesc>,
}

impl Catalog {
    fn insert(&mut self, kind: &str, description: ControlKindDesc) {
        self.kinds.insert(ControlKind(kind.to_owned()), description);
    }
}

impl ControlCatalog for Catalog {
    fn kind(&self, kind: &str) -> Option<&ControlKindDesc> {
        self.kinds.get(kind)
    }
}

static RENDER_CATALOG: LazyLock<Catalog> = LazyLock::new(build_catalog);

#[must_use]
pub fn catalog() -> impl ControlCatalog {
    build_catalog()
}

pub fn render<'a>(
    node: &CompiledNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    module_chrome(render_compiled(node, ui, reads, palette), palette)
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
        CompiledNode::Module { root, .. } => render_node(root, ui, reads, palette),
    }
}

fn render_node<'a>(
    node: &ExpandedNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let element = match node {
        ExpandedNode::Row { children, .. } => container(
            Row::with_children(
                children
                    .iter()
                    .map(|child| render_node(child, ui, reads, palette)),
            )
            .spacing(Consts::GRID_GAP)
            .width(Length::Fill),
        )
        .width(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.line_soft)))
        .into(),
        ExpandedNode::Column { children, .. } | ExpandedNode::Slot { children, .. } => container(
            Column::with_children(
                children
                    .iter()
                    .map(|child| render_node(child, ui, reads, palette)),
            )
            .spacing(Consts::GRID_GAP)
            .width(Length::Fill),
        )
        .width(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.line_soft)))
        .into(),
        ExpandedNode::Control {
            path,
            kind,
            props,
            read,
            ..
        } => render_control(*path, *kind, props, read.as_ref(), ui, reads, palette),
    };
    apply_size(element, effective_size(node, ui, &*RENDER_CATALOG))
}

fn render_control<'a>(
    path: InternId,
    kind: InternId,
    props: &BTreeMap<InternId, PropValue<InternId>>,
    read: Option<&Binding>,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let value = read.and_then(|binding| resolve(reads, binding, ui));
    let path = ui.resolve(path);
    match ui.resolve(kind) {
        "deck.header" => deck::header(
            text_prop(props, "badge", ui),
            value.as_ref(),
            reads,
            palette,
        ),
        "deck.summary" => deck::summary(
            text_prop(props, "style", ui),
            value.as_ref(),
            reads,
            palette,
        ),
        "global.brand" => global_bar::brand(palette),
        "global.spacer" => global_bar::spacer(palette),
        "preset.selector" => global_bar::preset_selector(reads, palette),
        "view.settings" => global_bar::settings_button(palette),
        "telemetry.bpm" => deck::bpm(
            text_prop(props, "fallback", ui),
            value.as_ref(),
            reads,
            palette,
        ),
        "telemetry.time" => deck::time(value.as_ref(), reads, palette),
        "text" => text::view(text_prop(props, "style", ui), value.as_ref(), palette),
        "button" => button::view(
            path,
            text_prop(props, "label", ui),
            text_prop(props, "active-label", ui),
            text_prop(props, "style", ui),
            value.as_ref(),
            palette,
        ),
        "telemetry.scalar" => {
            telemetry::view(text_prop(props, "format", ui), value.as_ref(), palette)
        }
        "fader.horizontal" => {
            fader::view(path, text_prop(props, "style", ui), value.as_ref(), palette)
        }
        "toggle" => toggle::toggle(path, value.as_ref(), palette),
        "checkbox" => toggle::checkbox(path, value.as_ref(), palette),
        "readout" => readout::view(
            text_prop(props, "label", ui),
            text_prop(props, "tone", ui),
            bool_prop(props, "framed", ui),
            value.as_ref(),
            palette,
        ),
        "chip" => chip::view(path, text_prop(props, "label", ui), value.as_ref(), palette),
        "knob" => knob::view(path, value.as_ref(), palette),
        "vu.stereo" => meter::view(path, value.as_ref(), palette),
        "vu.vertical" => vu::view(path, value.as_ref(), palette),
        "waveform.mini" => mini_wave::view(
            path,
            text_prop(props, "style", ui),
            value.as_ref(),
            reads,
            palette,
        ),
        "track_list" => track_list::view(path, value.as_ref(), reads, palette),
        _ => Space::new().into(),
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

fn bool_prop(props: &BTreeMap<InternId, PropValue<InternId>>, name: &str, ui: &CompiledUi) -> bool {
    props
        .iter()
        .find(|(key, _)| ui.resolve(**key) == name)
        .is_some_and(|(_, value)| matches!(value, PropValue::Bool(true)))
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
    };
    declared.or_else(|| match node {
        ExpandedNode::Control { kind, .. } => catalog
            .kind(ui.resolve(*kind))
            .map(|description| description.size),
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

fn build_catalog() -> Catalog {
    let mut catalog = Catalog::default();
    catalog.insert("deck.header", deck::header_desc());
    catalog.insert("deck.summary", deck::summary_desc());
    catalog.insert("global.brand", global_bar::brand_desc());
    catalog.insert("global.spacer", global_bar::spacer_desc());
    catalog.insert("preset.selector", global_bar::selector_desc());
    catalog.insert("view.settings", global_bar::settings_desc());
    catalog.insert("text", text::desc());
    catalog.insert("button", button::desc());
    catalog.insert("telemetry.bpm", deck::bpm_desc());
    catalog.insert("telemetry.time", deck::time_desc());
    catalog.insert("telemetry.scalar", telemetry::desc());
    catalog.insert("fader.horizontal", fader::desc());
    catalog.insert("toggle", toggle::toggle_desc());
    catalog.insert("checkbox", toggle::checkbox_desc());
    catalog.insert("readout", readout::desc());
    catalog.insert("chip", chip::desc());
    catalog.insert("knob", knob::desc());
    catalog.insert("vu.stereo", meter::desc());
    catalog.insert("vu.vertical", vu::desc());
    catalog.insert("waveform.mini", mini_wave::desc());
    catalog.insert("track_list", track_list::desc());
    catalog
}

#[cfg(test)]
mod tests {
    use iced::Size;
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
