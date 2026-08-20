use iced::{
    Alignment, Element, Length, Padding, Size, mouse,
    widget::{Column, Row, Space, Stack, container, mouse_area, scrollable},
};
use num_traits::cast::AsPrimitive;

use super::{
    control::render_control,
    geometry::{
        Rendered, active_tone, apply_size, bordered, content_size, effective_size, filled,
        frame_tone, length_for, padding,
    },
    read::{Answers, read_flag, resolve},
    size::{node_size, visible_children},
};
use crate::{
    compile::{CompiledNode, CompiledUi},
    expand::{ExpandedNode, MeasureSpec, SurfaceSpec},
    layout::Axis,
    module::{ChromeStyle, TextAlign},
    render::{ControlAction, DragPhase, ReadValue, Reads, Skin, UiEvent},
    size::{Dim, SizeSpec, branch, visible},
    widgets::{
        DropZone, ModuleChrome, Widget,
        adaptive::{Measured, Revealed, Shape},
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
    let snapshot = Answers { reads, ui };
    match node {
        CompiledNode::Optional { child, .. } => render_compiled(child, ui, reads, skin),
        CompiledNode::Adaptive {
            axis,
            size,
            base,
            steps,
        } => Measured::new(
            std::iter::once(base.as_ref())
                .chain(steps.iter().map(|(_, node)| node))
                .map(|node| render_compiled(node, ui, reads, skin))
                .collect(),
            steps.iter().map(|(from, _)| *from).collect(),
            *axis,
            Size::new(
                length_for(size.w, Length::Fill),
                length_for(size.h, Length::Fill),
            ),
        )
        .into(),
        CompiledNode::Split { axis, children, .. } => match axis {
            Axis::Horizontal => container(
                Row::with_children(
                    visible_children(children, &snapshot).map(|(weight, child)| {
                        container(render_compiled(child, ui, reads, skin))
                            .width(split_length(
                                node_size(child, skin.document(), &snapshot).w,
                                weight,
                                skin,
                            ))
                            .height(length_for(
                                node_size(child, skin.document(), &snapshot).h,
                                Length::Fill,
                            ))
                            .into()
                    }),
                )
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
            Axis::Vertical => container(
                Column::with_children(visible_children(children, &snapshot).map(
                    |(weight, child)| {
                        container(render_compiled(child, ui, reads, skin))
                            .width(Length::Fill)
                            .height(split_length(
                                node_size(child, skin.document(), &snapshot).h,
                                weight,
                                skin,
                            ))
                            .into()
                    },
                ))
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
    let snapshot = Answers { reads, ui };
    let size = content_size(node, skin, &snapshot);
    let rendered = match node {
        ExpandedNode::Adaptive {
            measure,
            size: Some(declared),
            base,
            steps,
        } => measured_branches(measure, (base, steps), *declared, (ui, reads, skin)),
        ExpandedNode::Adaptive {
            measure,
            base,
            steps,
            ..
        } => {
            return render_node(branch(measure, base, steps, &snapshot), ui, reads, skin);
        }
        ExpandedNode::Optional { child, .. } | ExpandedNode::Reveal { child, .. } => {
            return render_node(child, ui, reads, skin);
        }
        ExpandedNode::Row { .. } => render_row(node, size, ui, reads, skin),
        ExpandedNode::Column { .. } => render_column(node, size, ui, reads, skin),
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
        ExpandedNode::Scroll { child, .. } => render_scroll(child, size, ui, reads, skin),
        ExpandedNode::Slot { children, .. } => Rendered::leading(
            container(
                Column::with_children(
                    visible(children, &snapshot).map(|child| render_node(child, ui, reads, skin)),
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
    apply_size(rendered, effective_size(node, skin, &snapshot))
}

fn render_row<'a>(
    node: &ExpandedNode,
    size: (Length, Length),
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Rendered<'a> {
    let ExpandedNode::Row {
        children,
        measure,
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
    } = node
    else {
        unreachable!("render_row is called only for a row")
    };
    let snapshot = Answers { reads, ui };
    let active = read_flag(active.as_ref(), reads, ui);
    let gap = gap.unwrap_or(skin.layout.grid_gap);
    let inset = padding(*pad, *pad_x, *pad_y, skin);
    let (content, outer) = measure.map_or_else(
        || {
            let flow = Row::with_children(
                visible(children, &snapshot).map(|child| render_node(child, ui, reads, skin)),
            )
            .spacing(gap)
            .align_y(Alignment::Center)
            .width(size.0)
            .height(size.1);
            (Element::from(flow), inset)
        },
        |axis| {
            let shape = Shape {
                flow: Axis::Horizontal,
                measure: axis,
                size: Size::new(size.0, size.1),
                padding: inset,
                gap,
                align: Alignment::Center,
            };
            (revealed(children, shape, (ui, reads, skin)), Padding::ZERO)
        },
    );
    Rendered::leading(wheeled(
        bordered(
            filled(
                container(content)
                    .padding(outer)
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

fn render_column<'a>(
    node: &ExpandedNode,
    size: (Length, Length),
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Rendered<'a> {
    let ExpandedNode::Column {
        children,
        measure,
        gap,
        align,
        pad,
        pad_x,
        pad_y,
        frame,
        frame_color,
        background,
        background_alpha,
        surface,
        ..
    } = node
    else {
        unreachable!("render_column is called only for a column")
    };
    let snapshot = Answers { reads, ui };
    let gap = gap.unwrap_or(skin.layout.grid_gap);
    let inset = padding(*pad, *pad_x, *pad_y, skin);
    let (content, outer) = measure.map_or_else(
        || {
            let flow = Column::with_children(
                visible(children, &snapshot).map(|child| render_node(child, ui, reads, skin)),
            )
            .spacing(gap)
            .align_x(column_alignment(*align))
            .width(size.0);
            (Element::from(flow), inset)
        },
        |axis| {
            let shape = Shape {
                flow: Axis::Vertical,
                measure: axis,
                size: Size::new(size.0, size.1),
                padding: inset,
                gap,
                align: column_alignment(*align),
            };
            (revealed(children, shape, (ui, reads, skin)), Padding::ZERO)
        },
    );
    Rendered::leading(wheeled(
        bordered(
            filled(
                container(content)
                    .padding(outer)
                    .width(size.0)
                    .height(size.1),
                *background,
                *background_alpha,
                skin,
            ),
            *frame,
            frame_tone(*frame_color, None, false, skin),
            size,
            skin,
        ),
        surface.as_ref(),
        size,
        ui,
    ))
}

/// A container that measures itself hands the toolkit every child it lays out
/// with the threshold each appears at, and lets the box it is given decide
/// which of them stand. A child the host hides never reaches it.
fn revealed<'a>(
    children: &[ExpandedNode],
    shape: Shape,
    context: (&'a CompiledUi, &dyn Reads, &'a Skin),
) -> Element<'a, UiEvent> {
    let (ui, reads, skin) = context;
    let snapshot = Answers { reads, ui };
    let cells = visible(children, &snapshot)
        .map(|child| match child {
            ExpandedNode::Reveal { from, child } => (*from, render_node(child, ui, reads, skin)),
            child => (0.0, render_node(child, ui, reads, skin)),
        })
        .collect();
    Revealed::new(cells, shape).into()
}

/// A self-measured node hands every branch to the toolkit and lets the box it
/// is given pick one, so no branch is rebuilt when the pick changes.
fn measured_branches<'a>(
    measure: &MeasureSpec,
    branches: (&ExpandedNode, &[(f32, ExpandedNode)]),
    declared: SizeSpec,
    context: (&'a CompiledUi, &dyn Reads, &'a Skin),
) -> Rendered<'a> {
    let ((base, steps), (ui, reads, skin)) = (branches, context);
    let Some(axis) = measure.axis() else {
        return Rendered::leading(render_node(base, ui, reads, skin));
    };
    let elements = std::iter::once(base)
        .chain(steps.iter().map(|(_, node)| node))
        .map(|node| render_node(node, ui, reads, skin))
        .collect();
    let size = Size::new(
        length_for(declared.w, Length::Fill),
        length_for(declared.h, Length::Fill),
    );
    Rendered::leading(
        Measured::new(
            elements,
            steps.iter().map(|(from, _)| *from).collect(),
            axis,
            size,
        )
        .into(),
    )
}

fn render_scroll<'a>(
    child: &ExpandedNode,
    size: (Length, Length),
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Rendered<'a> {
    Rendered::leading(
        scrollable(render_node(child, ui, reads, skin))
            .width(size.0)
            .height(size.1)
            .into(),
    )
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
