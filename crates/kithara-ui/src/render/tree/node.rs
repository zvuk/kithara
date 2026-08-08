use iced::{
    Alignment, Element, Length, Size,
    widget::{Space, Stack, container, mouse_area},
};

use super::{
    control::render_control,
    flex::Flex,
    geometry::{Rendered, apply_size, bordered, filled, length_for, padding},
    host, read_flag,
};
#[cfg(test)]
use crate::compile::CompiledNode;
use crate::{
    compile::CompiledUi,
    expand::{Binding, ControlSpec, ExpandedNode, SurfaceSpec},
    ids::InternId,
    layout::Axis,
    module::TextAlign,
    render::{
        ControlAction, InputOwner, Reads, Skin, UiEvent,
        document::{
            Group, Host as DocumentHost, Module as DocumentModule, Popover as DocumentPopover,
        },
        window_layers,
    },
    size::{Dim, SizeSpec},
    widgets::{
        DropZone, ModuleChrome, Widget,
        anchored::{Anchored, Placement},
        wheel::WheelSurface,
    },
};

pub(super) struct IcedHost<'a, 'reads> {
    ui: &'a CompiledUi,
    reads: &'reads dyn Reads,
    skin: &'a Skin,
}

impl<'a, 'reads> IcedHost<'a, 'reads> {
    pub(super) const fn new(ui: &'a CompiledUi, reads: &'reads dyn Reads, skin: &'a Skin) -> Self {
        Self { ui, reads, skin }
    }
}

impl<'a> DocumentHost for IcedHost<'a, '_> {
    type Output = Element<'a, UiEvent>;

    fn split(&mut self, axis: Axis, children: Vec<(f32, SizeSpec, Self::Output)>) -> Self::Output {
        match axis {
            Axis::Horizontal => container(
                Flex::row_weighted(children.into_iter().map(|(weight, size, child)| {
                    (child, Size::new(main_length(size.w), Length::Fill), weight)
                }))
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
            Axis::Vertical => container(
                Flex::column_weighted(children.into_iter().map(|(weight, size, child)| {
                    (child, Size::new(Length::Fill, main_length(size.h)), weight)
                }))
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        }
    }

    fn module(
        &mut self,
        mut module: DocumentModule<'_>,
        content: Option<Self::Output>,
    ) -> Self::Output {
        let instance = self.ui.resolve(module.instance());
        let module_name = self.ui.resolve(module.module());
        let content = content.unwrap_or_else(|| Space::new().into());
        let chrome_hosted = module.chrome_hosted();
        let child = ModuleChrome::builder()
            .content(content)
            .module(module_name)
            .maybe_title(module.title().map(|id| self.ui.resolve(id)))
            .maybe_chip(module.chip().map(|id| self.ui.resolve(id)))
            .assign(
                module
                    .assign()
                    .iter()
                    .map(|id| self.ui.resolve(*id))
                    .collect(),
            )
            .style(module.chrome())
            .frame(module.frame())
            .corners(module.corners())
            .maybe_footer(module.take_footer())
            .input_owner(if chrome_hosted {
                InputOwner::Engine
            } else {
                InputOwner::Leaf
            })
            .maybe_drop(
                module
                    .drop()
                    .map(|drop| DropZone::new(read_flag(Some(&drop.read), self.reads, self.ui))),
            )
            .collapsed(module.collapsed())
            .skin(self.skin)
            .build()
            .view();
        if chrome_hosted {
            host::module_host(
                child,
                host::ModuleHost {
                    instance,
                    module: module_name,
                    chrome: module.chrome(),
                    collapsed: module.collapsed(),
                    drop: module.drop().is_some(),
                },
            )
        } else {
            child
        }
    }

    fn group(
        &mut self,
        group: Group<'_>,
        children: Vec<(Option<f32>, Self::Output)>,
    ) -> Self::Output {
        let size = content_size(group.size());
        let flex = match group.axis() {
            Axis::Horizontal => Flex::row(
                children
                    .into_iter()
                    .map(|(minimum, child)| (child, minimum)),
            )
            .spacing(group.gap())
            .align(column_alignment(group.alignment()))
            .width(size.0)
            .height(size.1),
            Axis::Vertical => Flex::column(
                children
                    .into_iter()
                    .map(|(minimum, child)| (child, minimum)),
            )
            .spacing(group.gap())
            .align(column_alignment(group.alignment()))
            .width(size.0),
        };
        let element = wheeled(
            bordered(
                filled(
                    container(flex)
                        .padding(padding(group.padding_x(), group.padding_y()))
                        .width(size.0)
                        .height(size.1),
                    group.background(),
                    group.background_alpha(),
                    self.skin,
                ),
                group.frame(),
                (group.frame_color(), group.frame_width()),
                size,
                self.skin,
            ),
            group.surface(),
            size,
            self.ui,
        );
        apply_size(Rendered::leading(element), group.size())
    }

    fn popover(
        &mut self,
        popover: DocumentPopover,
        anchor: Self::Output,
        content: Option<Self::Output>,
    ) -> Self::Output {
        let content = content.unwrap_or_else(|| Space::new().into());
        let element = Anchored::new(
            anchor,
            content,
            popover.is_open(),
            Placement {
                at: popover.at(),
                align: popover.align(),
            },
            crate::render::control_event(self.ui.resolve(popover.path()), ControlAction::Activate),
            self.skin,
        )
        .into();
        apply_size(Rendered::leading(element), popover.size())
    }

    fn pressable(
        &mut self,
        path: InternId,
        child: Self::Output,
        size: Option<SizeSpec>,
    ) -> Self::Output {
        let path = self.ui.resolve(path);
        let element = mouse_area(child)
            .on_press(crate::render::control_event(path, ControlAction::Activate))
            .on_right_press(crate::render::control_event(
                path,
                ControlAction::SecondaryActivate,
            ))
            .into();
        apply_size(Rendered::leading(element), size)
    }

    fn slot(&mut self, children: Vec<Self::Output>, size: Option<SizeSpec>) -> Self::Output {
        let element = container(
            Flex::column(children.into_iter().map(|child| (child, None)))
                .spacing(self.skin.layout.grid_gap)
                .width(Length::Fill),
        )
        .width(Length::Fill)
        .into();
        apply_size(Rendered::leading(element), size)
    }

    fn control(
        &mut self,
        path: InternId,
        spec: &ControlSpec,
        read: Option<&Binding>,
        owner: InputOwner,
        size: Option<SizeSpec>,
    ) -> Self::Output {
        apply_size(
            render_control(path, spec, read, self.ui, self.reads, self.skin, owner),
            size,
        )
    }

    fn hosted(&mut self, node: &ExpandedNode, child: Self::Output) -> Self::Output {
        host::host(child, node, self.ui, self.reads, self.skin)
    }

    fn window(
        &mut self,
        content: Self::Output,
        dragged: Option<String>,
        resize_edges: bool,
    ) -> Self::Output {
        if !resize_edges && dragged.is_none() {
            content
        } else {
            window_layers(content, dragged, resize_edges, self.skin)
        }
    }
}

fn main_length(dim: Dim) -> Length {
    match dim {
        Dim::Fixed(value) => Length::Fixed(value),
        _ => Length::Fill,
    }
}

fn content_size(size: Option<SizeSpec>) -> (Length, Length) {
    size.map_or((Length::Fill, Length::Fill), |size| {
        (
            length_for(size.w, Length::Fill),
            length_for(size.h, Length::Fill),
        )
    })
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

const fn column_alignment(align: TextAlign) -> Alignment {
    match align {
        TextAlign::Start => Alignment::Start,
        TextAlign::Center => Alignment::Center,
        TextAlign::End => Alignment::End,
    }
}

#[cfg(test)]
pub(super) fn render_compiled<'a>(
    node: &CompiledNode,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    crate::render::document::render(
        node,
        ui,
        reads,
        skin.document(),
        IcedHost::new(ui, reads, skin),
    )
}

#[cfg(test)]
pub(super) fn render_engine_node<'a>(
    node: &ExpandedNode,
    address: &[usize],
    owner: InternId,
    ui: &'a CompiledUi,
    reads: &dyn Reads,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    crate::render::document::render_engine_subtree(
        node,
        address,
        owner,
        ui,
        reads,
        skin.document(),
        IcedHost::new(ui, reads, skin),
    )
}
