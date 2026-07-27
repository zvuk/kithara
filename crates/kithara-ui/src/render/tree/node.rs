use iced::{
    Alignment, Element, Length,
    widget::{Column, Row, Space, container},
};
use num_traits::cast::AsPrimitive;

use super::{
    control::render_control,
    geometry::{Rendered, apply_size, bordered, content_size, effective_size, filled, padding},
    read::{read_flag, resolve},
};
use crate::{
    compile::{CompiledNode, CompiledUi},
    expand::ExpandedNode,
    layout::Axis,
    module::ChromeStyle,
    render::{ControlAction, DragPhase, ReadValue, Reads, Skin, UiEvent},
    size::{Dim, SizeSpec},
    widgets::{DropZone, ModuleChrome},
};

pub(super) fn render_compiled<'a>(
    node: &CompiledNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    match node {
        CompiledNode::Split { axis, children, .. } => match axis {
            Axis::Horizontal => container(
                Row::with_children(children.iter().map(|(weight, child)| {
                    container(render_compiled(child, ui, reads, skin))
                        .width(split_length(child_size(child).w, *weight, skin))
                        .height(Length::Fill)
                        .into()
                }))
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
            Axis::Vertical => container(
                Column::with_children(children.iter().map(|(weight, child)| {
                    container(render_compiled(child, ui, reads, skin))
                        .width(Length::Fill)
                        .height(split_length(child_size(child).h, *weight, skin))
                        .into()
                }))
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        },
        CompiledNode::Module {
            instance,
            module,
            title,
            chip,
            assign,
            chrome,
            frame,
            corners,
            footer,
            drop,
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
                .assign(assign.iter().map(|id| ui.resolve(*id)).collect())
                .style(*chrome)
                .frame(*frame)
                .corners(*corners)
                .maybe_footer(footer)
                .maybe_drop(drop.as_ref().map(|drop| {
                    module_drop_zone(
                        ui.resolve(*instance),
                        read_flag(Some(&drop.read), reads, ui),
                    )
                }))
                .on_toggle(UiEvent::ToggleModule(ui.resolve(*module).to_owned()))
                .collapsed(collapsed)
                .skin(skin)
                .build()
                .view()
        }
    }
}

fn module_drop_zone(instance: &str, active: bool) -> DropZone<UiEvent> {
    let crossing = |over| UiEvent::Control {
        path: format!("{instance}/drop"),
        action: ControlAction::Drag(DragPhase::Over(over)),
    };
    DropZone::new(crossing(true), crossing(false), active)
}

fn render_node<'a>(
    node: &ExpandedNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    let size = content_size(node, skin);
    let rendered = match node {
        ExpandedNode::Row {
            children,
            gap,
            pad,
            pad_x,
            pad_y,
            frame,
            background,
            background_alpha,
            ..
        } => Rendered::leading(bordered(
            filled(
                container(
                    Row::with_children(
                        children
                            .iter()
                            .map(|child| render_node(child, ui, reads, skin)),
                    )
                    .spacing(gap.unwrap_or(skin.layout.grid_gap))
                    .align_y(Alignment::Center)
                    .width(size.0)
                    .height(size.1),
                )
                .padding(padding(*pad, *pad_x, *pad_y, skin))
                .width(size.0)
                .height(size.1),
                *background,
                *background_alpha,
                skin,
            ),
            *frame,
            size,
            skin,
        )),
        ExpandedNode::Column {
            children,
            gap,
            pad,
            pad_x,
            pad_y,
            frame,
            background,
            background_alpha,
            ..
        } => Rendered::leading(bordered(
            filled(
                container(
                    Column::with_children(
                        children
                            .iter()
                            .map(|child| render_node(child, ui, reads, skin)),
                    )
                    .spacing(gap.unwrap_or(skin.layout.grid_gap))
                    .align_x(Alignment::Center)
                    .width(size.0),
                )
                .padding(padding(*pad, *pad_x, *pad_y, skin))
                .width(size.0)
                .height(size.1),
                *background,
                *background_alpha,
                skin,
            ),
            *frame,
            size,
            skin,
        )),
        ExpandedNode::Slot { children, .. } => Rendered::leading(
            container(
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
        ),
        ExpandedNode::Control {
            path, spec, read, ..
        } => render_control(*path, spec, read.as_ref(), ui, reads, skin),
    };
    apply_size(rendered, effective_size(node, skin))
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
