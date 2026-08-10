use iced::{
    Alignment, Element, Length, mouse,
    widget::{Column, Row, Space, Stack, container, mouse_area},
};
use num_traits::cast::AsPrimitive;

use super::{
    control::render_control,
    geometry::{
        Rendered, active_tone, apply_size, bordered, content_size, effective_size, filled,
        frame_tone, padding,
    },
    read::{read_flag, resolve},
    size::{node_size, visible_children},
};
use crate::{
    compile::{CompiledNode, CompiledUi},
    expand::{ExpandedNode, SurfaceSpec},
    layout::Axis,
    module::{ChromeStyle, TextAlign},
    render::{ControlAction, DragPhase, ReadValue, Reads, Skin, UiEvent},
    size::{Dim, Hidden, visible},
    widgets::{
        DropZone, ModuleChrome, Widget,
        anchored::{Anchored, Placement},
        wheel::WheelSurface,
    },
};

pub(super) fn render_compiled<'a>(
    node: &CompiledNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    let hidden: Hidden<'_> = &|block| read_flag(Some(&block.hidden), reads, ui);
    match node {
        CompiledNode::Optional { child, .. } => render_compiled(child, ui, reads, skin),
        CompiledNode::Split { axis, children, .. } => match axis {
            Axis::Horizontal => container(
                Row::with_children(visible_children(children, hidden).map(|(weight, child)| {
                    container(render_compiled(child, ui, reads, skin))
                        .width(split_length(
                            node_size(child, skin.document(), hidden).w,
                            weight,
                            skin,
                        ))
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
                Column::with_children(visible_children(children, hidden).map(|(weight, child)| {
                    container(render_compiled(child, ui, reads, skin))
                        .width(Length::Fill)
                        .height(split_length(
                            node_size(child, skin.document(), hidden).h,
                            weight,
                            skin,
                        ))
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

fn wheeled<'a>(
    element: Element<'a, UiEvent>,
    surface: Option<&SurfaceSpec>,
    size: (Length, Length),
    ui: &'a CompiledUi,
) -> Element<'a, UiEvent> {
    let Some(surface) = surface else {
        return element;
    };
    let wheel = WheelSurface::builder()
        .path(ui.resolve(surface.path))
        .build()
        .view();
    Stack::with_children([element, wheel])
        .width(size.0)
        .height(size.1)
        .into()
}

fn render_node<'a>(
    node: &ExpandedNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    let size = content_size(node, skin);
    let hidden: Hidden<'_> = &|block| read_flag(Some(&block.hidden), reads, ui);
    let rendered = match node {
        ExpandedNode::Optional { child, .. } => return render_node(child, ui, reads, skin),
        ExpandedNode::Row {
            children,
            gap,
            pad,
            pad_x,
            pad_y,
            frame,
            background,
            background_alpha,
            active,
            active_background,
            frame_color,
            active_frame_color,
            surface,
            ..
        } => {
            let active = read_flag(active.as_ref(), reads, ui);
            Rendered::leading(wheeled(
                bordered(
                    filled(
                        container(
                            Row::with_children(
                                visible(children, hidden)
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
                        active_tone(*background, *active_background, active),
                        *background_alpha,
                        skin,
                    ),
                    *frame,
                    frame_tone(*frame_color, *active_frame_color, active, skin),
                    size,
                    skin,
                ),
                surface.as_ref(),
                size,
                ui,
            ))
        }
        ExpandedNode::Column {
            children,
            gap,
            align,
            pad,
            pad_x,
            pad_y,
            frame,
            background,
            background_alpha,
            surface,
            ..
        } => Rendered::leading(wheeled(
            bordered(
                filled(
                    container(
                        Column::with_children(
                            visible(children, hidden)
                                .map(|child| render_node(child, ui, reads, skin)),
                        )
                        .spacing(gap.unwrap_or(skin.layout.grid_gap))
                        .align_x(column_alignment(*align))
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
                frame_tone(None, None, false, skin),
                size,
                skin,
            ),
            surface.as_ref(),
            size,
            ui,
        )),
        ExpandedNode::Popover {
            path,
            open,
            at,
            align,
            anchor,
            content,
        } => {
            let open = read_flag(Some(open), reads, ui);
            let content: Element<'a, UiEvent> = if open {
                render_node(content, ui, reads, skin)
            } else {
                Space::new().into()
            };
            Rendered::leading(
                Anchored::new(
                    render_node(anchor, ui, reads, skin),
                    content,
                    open,
                    Placement {
                        at: *at,
                        align: *align,
                    },
                    control_event(ui.resolve(*path), ControlAction::Activate),
                    skin,
                )
                .into(),
            )
        }
        ExpandedNode::Pressable { path, child, .. } => {
            let path = ui.resolve(*path);
            Rendered::leading(
                mouse_area(render_node(child, ui, reads, skin))
                    .interaction(mouse::Interaction::Pointer)
                    .on_press(control_event(path, ControlAction::Activate))
                    .on_right_press(control_event(path, ControlAction::SecondaryActivate))
                    .into(),
            )
        }
        ExpandedNode::Slot { children, .. } => Rendered::leading(
            container(
                Column::with_children(
                    visible(children, hidden).map(|child| render_node(child, ui, reads, skin)),
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

fn control_event(path: &str, action: ControlAction) -> UiEvent {
    UiEvent::Control {
        action,
        path: path.to_owned(),
    }
}

const fn column_alignment(align: TextAlign) -> Alignment {
    match align {
        TextAlign::Start => Alignment::Start,
        TextAlign::Center => Alignment::Center,
        TextAlign::End => Alignment::End,
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
