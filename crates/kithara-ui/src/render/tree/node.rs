use iced::{
    Alignment, Element, Length, Size,
    widget::{Space, Stack, container, mouse_area},
};

use super::{
    control::{Placed, render_control},
    geometry::{Rendered, apply_size, bordered, filled, length_for, padding},
    host,
    measure::{Flex, Measured},
};
#[cfg(test)]
use crate::compile::{Address, CompiledNode};
use crate::{
    compile::CompiledUi,
    draw::Transform,
    expand::{Binding, ControlSpec, ExpandedNode, SurfaceSpec},
    ids::InternId,
    layout::Axis,
    module::{MeasureAxis, TextAlign},
    render::{
        Anchored, ControlAction, DropZone, InputOwner, ModuleChrome, Placement, Skin, UiEvent,
        Viewport, WheelSurface, Widget,
        document::{
            Ctx, Group, GroupMount, Host as DocumentHost, Measured as MeasuredPlan,
            Module as DocumentModule, PlacedMount, Popover as DocumentPopover, SplitMount,
        },
        placed, window_layers,
    },
    size::{Dim, SizeSpec},
};

pub(super) struct IcedHost<'a, 'r> {
    skin: &'a Skin,
    ctx: Ctx<'a, 'r>,
}

impl<'a, 'r> IcedHost<'a, 'r> {
    pub(super) const fn new(ctx: Ctx<'a, 'r>, skin: &'a Skin) -> Self {
        Self { skin, ctx }
    }
}

impl<'a> DocumentHost for IcedHost<'a, '_> {
    type Output = Element<'a, UiEvent>;

    fn control(
        &mut self,
        path: InternId,
        spec: &ControlSpec,
        read: Option<&Binding>,
        owner: InputOwner,
        size: Option<SizeSpec>,
        transform: Transform,
    ) -> Self::Output {
        apply_size(
            render_control(
                path,
                spec,
                read,
                self.ctx,
                self.skin,
                Placed { owner, transform },
            ),
            size,
        )
    }

    fn group(&mut self, group: Group<'_>, children: Vec<GroupMount<Self::Output>>) -> Self::Output {
        let size = content_size(group.size());
        let flex = match group.axis() {
            Axis::Horizontal => Flex::row(
                children
                    .into_iter()
                    .map(|child| (child.output, child.minimum, child.band)),
            )
            .spacing(group.gap())
            .align(column_alignment(group.alignment()))
            .width(size.0)
            .height(size.1),
            Axis::Vertical => Flex::column(
                children
                    .into_iter()
                    .map(|child| (child.output, child.minimum, child.band)),
            )
            .spacing(group.gap())
            .align(column_alignment(group.alignment()))
            .width(size.0),
        }
        .measure(group.measure())
        .padding(padding(group.padding_x(), group.padding_y()));
        let lit = group.lit().filter(|lit| self.ctx.flag(Some(lit.flag())));
        let element = wheeled(
            bordered(
                filled(
                    container(flex).width(size.0).height(size.1),
                    lit.map_or_else(|| group.background(), |lit| lit.background()),
                    group.background_alpha(),
                    group.round(),
                    self.skin,
                ),
                group.frame(),
                (
                    lit.map_or_else(|| group.frame_color(), |lit| lit.frame_color()),
                    group.frame_width(),
                ),
                size,
                self.skin,
            ),
            group.surface(),
            size,
            self.ctx.ui,
        );
        apply_size(Rendered::leading(element), group.size())
    }

    fn hosted(&mut self, node: &ExpandedNode, child: Self::Output) -> Self::Output {
        host::host(child, node, self.ctx, self.skin)
    }

    fn measured(&mut self, plan: MeasuredPlan, branches: Vec<Self::Output>) -> Self::Output {
        let size = Size::new(
            length_for(plan.size.w, Length::Fill),
            length_for(plan.size.h, Length::Fill),
        );
        Measured::new(branches, plan, size).into()
    }

    fn module(
        &mut self,
        mut module: DocumentModule<'_>,
        content: Option<Self::Output>,
    ) -> Self::Output {
        let instance = self.ctx.ui.resolve(module.instance());
        let module_name = self.ctx.ui.resolve(module.module());
        let content = content.unwrap_or_else(|| Space::new().into());
        let chrome_hosted = module.chrome_hosted();
        let child = ModuleChrome::builder()
            .content(content)
            .module(module_name)
            .maybe_title(module.title().map(|id| self.ctx.ui.resolve(id)))
            .maybe_chip(module.chip().map(|id| self.ctx.ui.resolve(id)))
            .assign(
                module
                    .assign()
                    .iter()
                    .map(|id| self.ctx.ui.resolve(*id))
                    .collect(),
            )
            .style(module.chrome())
            .frame(module.frame())
            .corners(module.corners())
            .round(module.round())
            .maybe_footer(module.take_footer())
            .input_owner(if chrome_hosted {
                InputOwner::Engine
            } else {
                InputOwner::Leaf
            })
            .maybe_drop(
                module
                    .drop()
                    .map(|drop| DropZone::new(self.ctx.flag(Some(&drop.read)))),
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

    /// A placement is its own widget rather than a padded container: the
    /// pointer has to reach the child where it stands, and a container would
    /// have to take the whole scene to offer that room.
    fn placed(&mut self, placement: PlacedMount<'_>, child: Self::Output) -> Self::Output {
        placed(
            self.ctx.ui.resolve(placement.path).to_owned(),
            placement.at,
            placement.write.is_some(),
            placement.snap,
            child,
        )
    }

    fn popover(
        &mut self,
        popover: DocumentPopover<'_>,
        anchor: Self::Output,
        content: &mut dyn FnMut(&mut Self) -> Self::Output,
    ) -> Self::Output {
        let content = if popover.is_open() {
            content(self)
        } else {
            Space::new().into()
        };
        let element = Anchored::new(
            anchor,
            content,
            popover.is_open(),
            Placement {
                at: popover.at(),
                align: popover.align(),
            },
            crate::render::control_event(
                self.ctx.ui.resolve(popover.path()),
                ControlAction::Activate,
            ),
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
        let path = self.ctx.ui.resolve(path);
        let element = mouse_area(child)
            .on_press(crate::render::control_event(path, ControlAction::Activate))
            .on_right_press(crate::render::control_event(
                path,
                ControlAction::SecondaryActivate,
            ))
            .into();
        apply_size(Rendered::leading(element), size)
    }

    /// The viewport is the declared box; the child keeps whatever height it
    /// asked for and iced moves it under that window.
    ///
    /// The box is given to the viewport itself rather than to a container around
    /// it. A scrollable shrinks to its content, and a container told to fill
    /// would then centre that content in the leftover — a window whose first row
    /// starts below its own top, by half of whatever the page did not use.
    ///
    /// The window is this toolkit's own rather than the one iced ships: its
    /// offset is the same neutral window the retained host keeps, so both draw
    /// the indicator the skin describes instead of one host painting a bar of
    /// its own theme that the other has no counterpart for.
    fn scroll(
        &mut self,
        _id: InternId,
        child: Self::Output,
        size: Option<SizeSpec>,
    ) -> Self::Output {
        let (width, height) = content_size(size);
        Viewport::new(child, width, height, self.skin).into()
    }

    fn slot(
        &mut self,
        children: Vec<GroupMount<Self::Output>>,
        size: Option<SizeSpec>,
    ) -> Self::Output {
        let element = container(
            Flex::column(
                children
                    .into_iter()
                    .map(|child| (child.output, child.minimum, child.band)),
            )
            .spacing(self.skin.layout.grid_gap)
            .width(Length::Fill),
        )
        .width(Length::Fill)
        .into();
        apply_size(Rendered::leading(element), size)
    }

    fn split(
        &mut self,
        axis: Axis,
        measure: Option<MeasureAxis>,
        children: Vec<SplitMount<Self::Output>>,
    ) -> Self::Output {
        let flex = match axis {
            Axis::Horizontal => Flex::row_weighted(children.into_iter().map(|cell| {
                (
                    cell.output,
                    Size::new(main_length(cell.size.w), Length::Fill),
                    cell.weight,
                    cell.band,
                )
            })),
            Axis::Vertical => Flex::column_weighted(children.into_iter().map(|cell| {
                (
                    cell.output,
                    Size::new(Length::Fill, main_length(cell.size.h)),
                    cell.weight,
                    cell.band,
                )
            })),
        };
        container(
            flex.measure(measure)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    fn stage(&mut self, children: Vec<Self::Output>, size: Option<SizeSpec>) -> Self::Output {
        stage(children, size)
    }

    fn window(
        &mut self,
        content: Self::Output,
        carried: Option<&Binding>,
        resize_edges: bool,
    ) -> Self::Output {
        if !resize_edges && carried.is_none() {
            content
        } else {
            window_layers(content, self.ctx.label(carried), resize_edges, self.skin)
        }
    }
}

/// The layers a stage hands the toolkit.
///
/// The declared box goes to the stack itself rather than to a container around
/// it. `Stack::layout` resolves its own size from its width, height and first
/// layer, then offers every other layer that size loosely — which is the same
/// arithmetic `NodeLayout::Stage` runs on the retained host. Wrapping it instead
/// leaves the stack the width of its first layer inside a filled container, and
/// a wider child is then clipped to the first one: measured on the motion page,
/// where a 120-wide chip came out 107.
///
/// That first layer is offered the stack's own box tightly rather than loosely,
/// so whichever child a document happened to write first would be stretched to
/// the stage while its siblings kept the box they asked for. A layer that draws
/// nothing takes that offer, and every child the document wrote gets the one the
/// retained host makes: measured on the sprites page, where a 96-tall sprite
/// came out 112 tall here and 96 there, which a turn then carried 8 across the
/// screen.
fn stage<'a>(children: Vec<Element<'a, UiEvent>>, size: Option<SizeSpec>) -> Element<'a, UiEvent> {
    let Some(size) = size else {
        return Stack::with_children(children).into();
    };
    let mut layers: Vec<Element<'a, UiEvent>> = Vec::with_capacity(children.len() + 1);
    layers.push(Element::from(Space::new()));
    layers.extend(children);
    Stack::with_children(layers)
        .width(length_for(size.w, Length::Shrink))
        .height(length_for(size.h, Length::Shrink))
        .into()
}

fn main_length(dim: Dim) -> Length {
    match dim {
        Dim::Fixed(value) => Length::Fixed(value),
        _ => Length::Fill,
    }
}

/// The lengths a node hands the toolkit: its declared box where it has one,
/// and the whole room where it has none.
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
    ctx: Ctx<'a, '_>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    crate::render::document::render(node, ctx, IcedHost::new(ctx, skin))
}

#[cfg(test)]
pub(super) fn render_engine_node<'a>(
    node: &ExpandedNode,
    address: &Address<'_>,
    owner: InternId,
    ctx: Ctx<'a, '_>,
    skin: &'a Skin,
) -> Element<'a, UiEvent> {
    crate::render::document::render_engine_subtree(
        node,
        address,
        owner,
        ctx,
        IcedHost::new(ctx, skin),
    )
}

#[cfg(test)]
mod tests {
    use iced::{
        Pixels, Renderer,
        advanced::{
            layout::{Layout, Limits},
            widget::Tree,
        },
    };
    use iced_renderer::fallback::Renderer as FallbackRenderer;
    use iced_tiny_skia::Renderer as TinySkiaRenderer;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        ids::{Interner, SourceUri},
        render::{ReadValue, Reads, document::probe, fonts::SANS},
    };

    #[kithara::test]
    fn a_node_declaring_no_box_takes_the_whole_room() {
        assert_eq!(content_size(None), (Length::Fill, Length::Fill));
    }

    #[kithara::test]
    fn a_declared_box_maps_each_axis_on_its_own() {
        assert_eq!(
            content_size(Some(SizeSpec::new(Dim::Fixed(40.0), Dim::Shrink))),
            (Length::Fixed(40.0), Length::Shrink)
        );
    }

    /// The box each child of the stage asks for. The stage itself is taller,
    /// which is the only way a child being stretched to it shows up at all.
    const CHILD: f32 = 96.0;

    /// The boxes a stage lays the children a document wrote into.
    ///
    /// Read from the end, so this says nothing about what a stage may put
    /// underneath them and measures only what the document asked for.
    fn document_children(count: usize) -> Vec<Size> {
        let children = (0..count)
            .map(|_| {
                apply_size(
                    Rendered::leading(Space::new().into()),
                    Some(SizeSpec::new(Dim::Fixed(CHILD), Dim::Fixed(CHILD))),
                )
            })
            .collect();
        let stage_height = CHILD + 16.0;
        let mut element = stage(
            children,
            Some(SizeSpec::new(Dim::Fill, Dim::Fixed(stage_height))),
        );
        let renderer: Renderer =
            FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)));
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, Size::new(600.0, stage_height)),
        );
        let boxes: Vec<Size> = Layout::new(&node)
            .children()
            .map(|child| child.bounds().size())
            .collect();

        boxes[boxes.len() - count..].to_vec()
    }

    /// The toolkit offers a stack's first layer the stack's own box tightly and
    /// every later one loosely, which would otherwise stretch whichever child a
    /// document happened to write first.
    #[kithara::test]
    fn a_stage_lays_two_children_that_asked_for_one_box_into_the_same_box() {
        let boxes = document_children(2);

        assert_eq!(boxes[0], boxes[1]);
    }

    #[kithara::test]
    fn the_child_a_document_wrote_first_keeps_the_box_it_asked_for() {
        assert_eq!(document_children(2)[0], Size::new(CHILD, CHILD));
    }

    /// A stage with one child has no sibling to disagree with, so this is the
    /// only place the stretch shows as itself.
    #[kithara::test]
    fn a_stages_only_child_keeps_the_box_it_asked_for() {
        assert_eq!(document_children(1)[0], Size::new(CHILD, CHILD));
    }

    /// A viewport is a window, and a window's first row is at its own top.
    ///
    /// Sized through a container instead, a scrollable shorter than its
    /// declared box is centred in what is left over, and the page it holds
    /// starts halfway down a gap nobody wrote — which is neither what the
    /// document says nor what the retained host does.
    #[kithara::test]
    fn a_viewport_puts_its_content_at_its_own_top() {
        struct Silent;

        impl Reads for Silent {
            fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
                None
            }
        }

        let window = CHILD + 64.0;
        let silent = Silent;
        let mut host = IcedHost::new(probe(&silent), builtin::skin());
        let mut interner = Interner::new(1024);
        let id = interner
            .intern("pages", &SourceUri("tree-test.ron".to_owned()))
            .unwrap_or_else(|error| panic!("the viewport path must intern: {error}"));
        let child = apply_size(
            Rendered::leading(Space::new().into()),
            Some(SizeSpec::new(Dim::Fill, Dim::Fixed(CHILD))),
        );
        let mut element = host.scroll(id, child, Some(SizeSpec::new(Dim::Fill, Dim::Fill)));
        let renderer: Renderer =
            FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)));
        let mut tree = Tree::new(element.as_widget());

        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, Size::new(600.0, window)),
        );

        let content = Layout::new(&node)
            .children()
            .next()
            .expect("the viewport lays out the content it was given");
        assert_eq!(content.bounds().y, 0.0);
    }
}
