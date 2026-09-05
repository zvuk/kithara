use iced::{
    Alignment, Element, Event, Length, Padding, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Clipboard, Shell, Widget as IcedWidget,
        layout::{self, Layout, Node as LayoutNode},
        mouse, overlay, renderer,
        widget::{self, Operation, Tree},
    },
};

use crate::{
    layout::Axis,
    module::MeasureAxis,
    render::{
        UiEvent,
        document::{Band, Measured as Plan},
    },
    solve::{self, Distribution, Input, Measure},
};

pub(super) struct Flex<'a> {
    align: Alignment,
    axis: Axis,
    height: Length,
    width: Length,
    /// The axis whose room decides which children stand, when they come and go
    /// with the room at all.
    measure: Option<MeasureAxis>,
    padding: Padding,
    child_layouts: Vec<ChildLayout>,
    children: Vec<Element<'a, UiEvent>>,
    spacing: f32,
}

#[derive(Clone, Copy)]
struct ChildLayout {
    band: Band,
    declared: Option<Size<Length>>,
    main_minimum: Option<f32>,
    main_weight: Option<f32>,
}

/// The storage one flow keeps between layout passes: which children stood the
/// last time the room was measured, and which slots the solver was given. Both
/// are refilled in place, because a flow is laid out again for every frame that
/// resizes it and its widget is rebuilt from the document each time.
#[derive(Default)]
struct State {
    shown: Vec<bool>,
    /// Which child each item the solver asks about actually is.
    slots: Vec<usize>,
}

impl<'a> Flex<'a> {
    pub(super) fn align(mut self, alignment: Alignment) -> Self {
        self.align = alignment;
        self
    }

    pub(super) fn column(
        children: impl IntoIterator<Item = (Element<'a, UiEvent>, Option<f32>, Band)>,
    ) -> Self {
        Self::with_children(
            Axis::Vertical,
            children
                .into_iter()
                .map(|(child, main_minimum, band)| (child, None, main_minimum, None, band)),
        )
    }

    pub(super) fn column_weighted(
        children: impl IntoIterator<Item = (Element<'a, UiEvent>, Size<Length>, f32, Band)>,
    ) -> Self {
        Self::weighted(Axis::Vertical, children)
    }

    pub(super) fn height(mut self, height: Length) -> Self {
        self.height = height;
        self
    }

    /// Names the axis whose room decides which children stand.
    pub(super) const fn measure(mut self, axis: Option<MeasureAxis>) -> Self {
        self.measure = axis;
        self
    }

    /// The room this flow keeps for itself around its children.
    ///
    /// The flow owns its padding rather than sitting inside a padded container,
    /// so a band is read against the box the document declared. See
    /// `render/masonry/flex.rs`, which reads the same number.
    pub(super) const fn padding(mut self, padding: Padding) -> Self {
        self.padding = padding;
        self
    }

    fn push(
        mut self,
        child: Element<'a, UiEvent>,
        declared: Option<Size<Length>>,
        main_minimum: Option<f32>,
        main_weight: Option<f32>,
        band: Band,
    ) -> Self {
        let size_hint = declared.unwrap_or_else(|| child.as_widget().size_hint());

        if !size_hint.is_void() {
            self.width = self.width.enclose(size_hint.width);
            self.height = self.height.enclose(size_hint.height);
            self.children.push(child);
            self.child_layouts.push(ChildLayout {
                band,
                declared,
                main_minimum,
                main_weight,
            });
        }

        self
    }

    pub(super) fn row(
        children: impl IntoIterator<Item = (Element<'a, UiEvent>, Option<f32>, Band)>,
    ) -> Self {
        Self::with_children(
            Axis::Horizontal,
            children
                .into_iter()
                .map(|(child, main_minimum, band)| (child, None, main_minimum, None, band)),
        )
    }

    pub(super) fn row_weighted(
        children: impl IntoIterator<Item = (Element<'a, UiEvent>, Size<Length>, f32, Band)>,
    ) -> Self {
        Self::weighted(Axis::Horizontal, children)
    }

    fn weighted(
        axis: Axis,
        children: impl IntoIterator<Item = (Element<'a, UiEvent>, Size<Length>, f32, Band)>,
    ) -> Self {
        Self::with_children(
            axis,
            children
                .into_iter()
                .map(|(child, declared, main_weight, band)| {
                    (child, Some(declared), None, Some(main_weight), band)
                }),
        )
    }

    pub(super) fn spacing(mut self, spacing: f32) -> Self {
        self.spacing = spacing;
        self
    }

    pub(super) fn width(mut self, width: Length) -> Self {
        self.width = width;
        self
    }

    fn with_children(
        axis: Axis,
        children: impl IntoIterator<
            Item = (
                Element<'a, UiEvent>,
                Option<Size<Length>>,
                Option<f32>,
                Option<f32>,
                Band,
            ),
        >,
    ) -> Self {
        let iterator = children.into_iter();
        let capacity = iterator.size_hint().0;
        let mut flex = Self {
            axis,
            measure: None,
            spacing: 0.0,
            padding: Padding::ZERO,
            width: Length::Shrink,
            height: Length::Shrink,
            align: Alignment::Start,
            children: Vec::with_capacity(capacity),
            child_layouts: Vec::with_capacity(capacity),
        };

        for (child, declared, main_minimum, main_weight, band) in iterator {
            flex = flex.push(child, declared, main_minimum, main_weight, band);
        }

        flex
    }
}

/// What a flow remembers between passes, and how it reads it back. A flow is
/// built afresh from the document for every view, so the storage below lives
/// in the state its tree keeps for it rather than in the flow itself.
impl<'a> Flex<'a> {
    /// Refills what this flow keeps between passes: which children the room
    /// reached, and which child each item the solver asks about is.
    fn record(&self, limits: &layout::Limits, state: &mut State) {
        self.stand(limits, &mut state.shown);
        state.slots.clear();
        state.slots.extend(
            state
                .shown
                .iter()
                .enumerate()
                .filter_map(|(index, on)| on.then_some(index)),
        );
    }

    /// The children that stood the last time this flow was laid out, beside
    /// their state and their box.
    fn shown<'t>(
        &'t self,
        tree: &'t Tree,
        layout: Layout<'t>,
    ) -> impl Iterator<Item = (&'t Element<'a, UiEvent>, &'t Tree, Layout<'t>)> {
        let stood = stood(&tree.state, self.children.len());
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .enumerate()
            .filter_map(move |(index, ((child, state), bounds))| {
                standing(stood, index).then_some((child, state, bounds))
            })
    }

    fn shown_mut<'t>(
        &'t mut self,
        tree: &'t mut Tree,
        layout: Layout<'t>,
    ) -> impl Iterator<Item = (&'t mut Element<'a, UiEvent>, &'t mut Tree, Layout<'t>)> {
        let Tree {
            state, children, ..
        } = tree;
        let stood = stood(state, self.children.len());
        self.children
            .iter_mut()
            .zip(children)
            .zip(layout.children())
            .enumerate()
            .filter_map(move |(index, ((child, state), bounds))| {
                standing(stood, index).then_some((child, state, bounds))
            })
    }

    /// Records in `into` which children stand in the room this flow turned out
    /// to have. A flow that measures nothing stands all of them.
    fn stand(&self, limits: &layout::Limits, into: &mut Vec<bool>) {
        into.clear();
        let Some(axis) = self.measure else {
            into.resize(self.children.len(), true);
            return;
        };
        let room = match axis {
            MeasureAxis::Width => limits.max().width,
            MeasureAxis::Height => limits.max().height,
        };
        into.extend(
            self.child_layouts
                .iter()
                .map(|child| child.band.stands(room)),
        );
    }
}

/// The padding a box of this room can actually spend.
fn fitted_padding(padding: Padding, room: Size) -> Padding {
    let left = padding.left.min(room.width);
    let top = padding.top.min(room.height);
    Padding {
        top,
        right: padding.right.min(room.width - left),
        bottom: padding.bottom.min(room.height - top),
        left,
    }
}

/// What the last layout recorded, or nothing when the recording does not
/// describe this many children: a flow that has not been laid out yet stands
/// all of them, so a missing reading is every child rather than none.
fn stood(state: &widget::tree::State, count: usize) -> Option<&[bool]> {
    let shown = &state.downcast_ref::<State>().shown;
    (shown.len() == count).then_some(shown.as_slice())
}

fn standing(stood: Option<&[bool]>, index: usize) -> bool {
    stood.is_none_or(|shown| shown[index])
}

impl IcedWidget<UiEvent, Theme, Renderer> for Flex<'_> {
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if layout.bounds().intersection(viewport).is_some() {
            for (child, tree, layout) in self
                .shown(tree, layout)
                .filter(|(_, _, layout)| layout.bounds().intersects(viewport))
            {
                child
                    .as_widget()
                    .draw(tree, renderer, theme, style, layout, cursor, viewport);
            }
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let limits = match self.axis {
            Axis::Horizontal => *limits,
            Axis::Vertical => limits.max_width(f32::INFINITY),
        };
        let Tree {
            state,
            children: trees,
            ..
        } = tree;
        let state = state.downcast_mut::<State>();
        self.record(&limits, state);
        let State { shown, slots } = &*state;
        let padding = fitted_padding(self.padding, limits.max());
        let solver_limits = limits.into();
        let items = slots
            .iter()
            .map(|slot| {
                let child = &self.child_layouts[*slot];
                let declared = child
                    .declared
                    .unwrap_or_else(|| self.children[*slot].as_widget().size());
                child.main_weight.map_or_else(
                    || solve::Item::new(declared.into(), child.main_minimum),
                    |weight| solve::Item::weighted(declared.into(), weight),
                )
            })
            .collect::<Vec<solve::Item>>();
        let mut measure = IcedMeasure {
            slots,
            trees,
            renderer,
            children: &mut self.children,
            child_layouts: &self.child_layouts,
            nodes: vec![layout::Node::default(); shown.len()],
        };
        let Distribution {
            size,
            items: placements,
        } = solve::resolve(
            Input {
                items,
                axis: self.axis,
                limits: &solver_limits,
                width: self.width.into(),
                height: self.height.into(),
                padding: padding.into(),
                spacing: self.spacing,
                align_items: self.align.into(),
            },
            &mut measure,
        );
        let mut nodes = measure.nodes;

        for (slot, placement) in slots.iter().zip(placements) {
            nodes[*slot].move_to_mut(placement.offset);
        }

        layout::Node::with_children(size.into(), nodes)
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.shown(tree, layout)
            .map(|(child, tree, layout)| {
                child
                    .as_widget()
                    .mouse_interaction(tree, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            self.children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
                .for_each(|((child, state), layout)| {
                    child
                        .as_widget_mut()
                        .operate(state, layout, renderer, operation);
                });
        });
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, UiEvent, Theme, Renderer>> {
        let floating: Vec<_> = self
            .shown_mut(tree, layout)
            .filter_map(|(child, tree, layout)| {
                child
                    .as_widget_mut()
                    .overlay(tree, layout, renderer, viewport, translation)
            })
            .collect();
        (!floating.is_empty()).then(|| overlay::Group::with_children(floating).overlay())
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn size_hint(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(State::default())
    }

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State>()
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, UiEvent>,
        viewport: &Rectangle,
    ) {
        for (child, tree, layout) in self.shown_mut(tree, layout) {
            child.as_widget_mut().update(
                tree, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }
}

impl<'a> From<Flex<'a>> for Element<'a, UiEvent> {
    fn from(flex: Flex<'a>) -> Self {
        Self::new(flex)
    }
}

impl From<Alignment> for solve::Alignment {
    fn from(alignment: Alignment) -> Self {
        match alignment {
            Alignment::Start => Self::Start,
            Alignment::Center => Self::Center,
            Alignment::End => Self::End,
        }
    }
}

impl From<Length> for solve::Length {
    fn from(length: Length) -> Self {
        match length {
            Length::Fill => Self::Fill,
            Length::FillPortion(factor) => Self::FillPortion(factor),
            Length::Shrink => Self::Shrink,
            Length::Fixed(amount) => Self::Fixed(amount),
        }
    }
}

impl From<Padding> for solve::Padding {
    fn from(padding: Padding) -> Self {
        Self {
            top: padding.top,
            right: padding.right,
            bottom: padding.bottom,
            left: padding.left,
        }
    }
}

impl From<solve::Point> for iced::Point {
    fn from(point: solve::Point) -> Self {
        Self::new(point.x, point.y)
    }
}

impl<T, U> From<Size<T>> for solve::Size<U>
where
    U: From<T>,
{
    fn from(size: Size<T>) -> Self {
        Self::new(U::from(size.width), U::from(size.height))
    }
}

impl<T, U> From<solve::Size<T>> for Size<U>
where
    U: From<T>,
{
    fn from(size: solve::Size<T>) -> Self {
        Self::new(U::from(size.width), U::from(size.height))
    }
}

impl From<layout::Limits> for solve::Limits {
    fn from(limits: layout::Limits) -> Self {
        Self::with_compression(
            limits.min().into(),
            limits.max().into(),
            limits.compression().into(),
        )
    }
}

impl From<solve::Limits> for layout::Limits {
    fn from(limits: solve::Limits) -> Self {
        Self::with_compression(
            limits.min().into(),
            limits.max().into(),
            limits.compression().into(),
        )
    }
}

struct IcedMeasure<'a, 'element> {
    renderer: &'a Renderer,
    child_layouts: &'a [ChildLayout],
    children: &'a mut [Element<'element, UiEvent>],
    trees: &'a mut [Tree],
    /// Which child each item the solver asks about actually is: the solver sees
    /// only the cells that stand.
    slots: &'a [usize],
    nodes: Vec<layout::Node>,
}

impl Measure for IcedMeasure<'_, '_> {
    fn measure(&mut self, index: usize, limits: &solve::Limits) -> solve::Size {
        let slot = self.slots[index];
        let declared: Option<solve::Size<solve::Length>> =
            self.child_layouts[slot].declared.map(Into::into);
        let child_limits =
            declared.map(|declared| limits.width(declared.width).height(declared.height).loose());
        let child_limits = child_limits.unwrap_or(*limits).into();
        let node = self.children[slot].as_widget_mut().layout(
            &mut self.trees[slot],
            self.renderer,
            &child_limits,
        );
        let intrinsic = node.size().into();
        let size = declared.map_or_else(
            || intrinsic,
            |declared| {
                limits
                    .width(declared.width)
                    .height(declared.height)
                    .resolve(declared.width, declared.height, intrinsic)
            },
        );
        self.nodes[slot] = node;
        size
    }
}

/// The branches of a node that draws whichever one fits its room.
///
/// Every branch is mounted, because the choice belongs to the layout pass: only
/// the branch that stands is measured, drawn, and driven, and the others keep an
/// empty node so a branch holds the same place from one frame to the next.
pub(super) struct Measured<'a> {
    plan: Plan,
    size: Size<Length>,
    branches: Vec<Element<'a, UiEvent>>,
}

#[derive(Default)]
struct Drawn {
    drawn: usize,
}

impl<'a> Measured<'a> {
    pub(super) fn new(branches: Vec<Element<'a, UiEvent>>, plan: Plan, size: Size<Length>) -> Self {
        Self {
            plan,
            size,
            branches,
        }
    }

    fn drawn(&self, tree: &Tree) -> usize {
        tree.state
            .downcast_ref::<Drawn>()
            .drawn
            .min(self.branches.len().saturating_sub(1))
    }

    fn pick(&self, room: Size) -> usize {
        let value = match self.plan.axis {
            MeasureAxis::Width => room.width,
            MeasureAxis::Height => room.height,
        };
        self.plan
            .branch(value)
            .min(self.branches.len().saturating_sub(1))
    }
}

impl IcedWidget<UiEvent, Theme, Renderer> for Measured<'_> {
    fn children(&self) -> Vec<Tree> {
        self.branches.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.branches);
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let drawn = self.drawn(tree);
        let Some(bounds) = layout.children().nth(drawn) else {
            return;
        };
        self.branches[drawn].as_widget().draw(
            &tree.children[drawn],
            renderer,
            theme,
            style,
            bounds,
            cursor,
            viewport,
        );
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let limits = limits.width(self.size.width).height(self.size.height);
        let drawn = self.pick(limits.max());
        tree.state.downcast_mut::<Drawn>().drawn = drawn;

        let mut nodes = vec![LayoutNode::default(); self.branches.len()];
        let node = self.branches[drawn].as_widget_mut().layout(
            &mut tree.children[drawn],
            renderer,
            &limits,
        );
        let size = limits.resolve(self.size.width, self.size.height, node.size());
        nodes[drawn] = node;
        LayoutNode::with_children(size, nodes)
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let drawn = self.drawn(tree);
        layout
            .children()
            .nth(drawn)
            .map_or_else(mouse::Interaction::default, |bounds| {
                self.branches[drawn].as_widget().mouse_interaction(
                    &tree.children[drawn],
                    bounds,
                    cursor,
                    viewport,
                    renderer,
                )
            })
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, UiEvent, Theme, Renderer>> {
        let drawn = self.drawn(tree);
        let bounds = layout.children().nth(drawn)?;
        self.branches[drawn].as_widget_mut().overlay(
            &mut tree.children[drawn],
            bounds,
            renderer,
            viewport,
            translation,
        )
    }

    fn size(&self) -> Size<Length> {
        self.size
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(Drawn::default())
    }

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<Drawn>()
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, UiEvent>,
        viewport: &Rectangle,
    ) {
        let drawn = self.drawn(tree);
        let Some(bounds) = layout.children().nth(drawn) else {
            return;
        };
        self.branches[drawn].as_widget_mut().update(
            &mut tree.children[drawn],
            event,
            bounds,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }
}

impl<'a> From<Measured<'a>> for Element<'a, UiEvent> {
    fn from(measured: Measured<'a>) -> Self {
        Self::new(measured)
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use iced::{
        Pixels,
        advanced::{graphics::text::font_system, layout::Limits, widget::tree::Tag},
        widget::Space,
    };
    use iced_renderer::fallback::Renderer as FallbackRenderer;
    use iced_tiny_skia::Renderer as TinySkiaRenderer;
    use kithara_test_utils::kithara;

    use super::{
        Alignment, Band, Element, Flex, IcedWidget, Layout, Length, MeasureAxis, Measured, Padding,
        Plan, Renderer, Size, State, Theme, Tree, UiEvent,
    };
    use crate::{
        render::fonts::{FONT_BYTES, SANS},
        size::SizeSpec,
    };

    const GAP: f32 = 4.0;

    fn renderer() -> Renderer {
        let mut fonts = font_system()
            .write()
            .expect("iced font system lock must not be poisoned");
        for bytes in FONT_BYTES {
            fonts.load_font(Cow::Borrowed(bytes));
        }
        drop(fonts);

        FallbackRenderer::Secondary(TinySkiaRenderer::new(SANS, Pixels(14.0)))
    }

    fn cell(width: f32) -> Element<'static, UiEvent> {
        Space::new().width(width).height(20.0).into()
    }

    fn portion(weight: u16) -> Element<'static, UiEvent> {
        Space::new()
            .width(Length::FillPortion(weight))
            .height(Length::Fill)
            .into()
    }

    /// The bar main calibrated its thresholds against: a strip that always
    /// stands, a cell from 440, and one from 350.
    fn bar() -> Flex<'static> {
        Flex::row([
            (cell(10.0), None, Band::ALWAYS),
            (cell(20.0), None, Band::new(440.0, None)),
            (cell(30.0), None, Band::new(350.0, None)),
        ])
        .spacing(GAP)
        .align(Alignment::Center)
        .width(Length::Fill)
        .height(Length::Fixed(42.0))
        .measure(Some(MeasureAxis::Width))
    }

    fn tree_for(flex: &Flex<'_>) -> Tree {
        Tree {
            tag: Tag::of::<State>(),
            state: IcedWidget::<UiEvent, Theme, Renderer>::state(flex),
            children: IcedWidget::<UiEvent, Theme, Renderer>::children(flex),
        }
    }

    /// Where each child sits on the flow's own axis, and how much of it it took.
    fn lay_out(flex: &mut Flex<'_>, tree: &mut Tree, room: f32) -> (Size, Vec<(f32, f32)>) {
        let node = flex.layout(
            tree,
            &renderer(),
            &Limits::new(Size::ZERO, Size::new(room, 42.0)),
        );
        let boxes = Layout::new(&node)
            .children()
            .map(|child| (child.bounds().x, child.bounds().width))
            .collect();
        (node.size(), boxes)
    }

    #[kithara::test]
    fn a_measuring_row_gives_its_cells_the_portions_a_plain_row_gives() {
        let room = 400.0;
        let mut measuring = Flex::row([
            (portion(1), None, Band::ALWAYS),
            (portion(3), None, Band::ALWAYS),
            (portion(4), None, Band::new(900.0, None)),
        ])
        .width(Length::Fill)
        .height(Length::Fixed(42.0))
        .measure(Some(MeasureAxis::Width));
        let mut plain = Flex::row([
            (portion(1), None, Band::ALWAYS),
            (portion(3), None, Band::ALWAYS),
        ])
        .width(Length::Fill)
        .height(Length::Fixed(42.0));
        let mut measuring_tree = tree_for(&measuring);
        let mut plain_tree = tree_for(&plain);

        let (_, measured) = lay_out(&mut measuring, &mut measuring_tree, room);
        let (_, laid_out) = lay_out(&mut plain, &mut plain_tree, room);

        assert_eq!(laid_out, vec![(0.0, 100.0), (100.0, 300.0)]);
        assert_eq!(
            measured,
            vec![(0.0, 100.0), (100.0, 300.0), (0.0, 0.0)],
            "the cell waiting for a room the bar does not have takes none of it",
        );
    }

    #[kithara::test]
    fn a_cell_appears_at_its_own_threshold() {
        let mut flex = bar();
        let mut tree = tree_for(&flex);

        let (_, narrow) = lay_out(&mut flex, &mut tree, 300.0);
        let (_, wide) = lay_out(&mut flex, &mut tree, 500.0);

        assert_eq!(narrow, vec![(0.0, 10.0), (0.0, 0.0), (0.0, 0.0)]);
        assert_eq!(wide, vec![(0.0, 10.0), (14.0, 20.0), (38.0, 30.0)]);
    }

    #[kithara::test]
    fn a_hidden_cell_charges_no_gap() {
        let mut flex = bar();
        let mut tree = tree_for(&flex);

        let (_, boxes) = lay_out(&mut flex, &mut tree, 400.0);

        assert_eq!(boxes, vec![(0.0, 10.0), (0.0, 0.0), (14.0, 30.0)]);
    }

    #[kithara::test]
    fn a_pass_that_reveals_some_cells_leaves_the_order_it_found() {
        let mut flex = bar();
        let mut tree = tree_for(&flex);

        lay_out(&mut flex, &mut tree, 400.0);
        let (_, boxes) = lay_out(&mut flex, &mut tree, 500.0);

        assert_eq!(boxes, vec![(0.0, 10.0), (14.0, 20.0), (38.0, 30.0)]);
    }

    #[kithara::test]
    fn a_band_hands_the_line_over_at_the_number_it_ends_on() {
        let mut flex = Flex::row([
            (cell(10.0), None, Band::new(0.0, Some(350.0))),
            (cell(30.0), None, Band::new(350.0, None)),
        ])
        .spacing(GAP)
        .align(Alignment::Center)
        .width(Length::Fill)
        .height(Length::Fixed(42.0))
        .measure(Some(MeasureAxis::Width));
        let mut tree = tree_for(&flex);

        let (_, below) = lay_out(&mut flex, &mut tree, 349.0);
        let (_, reached) = lay_out(&mut flex, &mut tree, 350.0);

        assert_eq!(
            below,
            vec![(0.0, 10.0), (0.0, 0.0)],
            "349 is still the strip"
        );
        assert_eq!(reached, vec![(0.0, 0.0), (0.0, 30.0)], "350 is the wave");
    }

    /// The room a threshold is read against is the box the flow was given, not
    /// what is left of it once its own padding is spent. `render/masonry/flex.rs`
    /// reads the same number, so a document means one thing on both hosts.
    #[kithara::test]
    fn a_threshold_is_read_against_the_declared_box() {
        let mut flex = bar().padding(Padding::ZERO.left(30.0).right(30.0));
        let mut tree = tree_for(&flex);

        let (size, boxes) = lay_out(&mut flex, &mut tree, 360.0);

        assert_eq!(size, Size::new(360.0, 42.0), "the box it answers");
        assert_eq!(boxes[2].1, 30.0, "360 reaches 350 before padding is spent");
    }

    fn measured(axis: MeasureAxis) -> Measured<'static> {
        let branch = || Element::<UiEvent>::from(Space::new());
        Measured::new(
            vec![branch(), branch(), branch()],
            Plan {
                axis,
                steps: vec![100.0, 200.0],
                size: SizeSpec::FILL,
            },
            Size::new(Length::Fill, Length::Fill),
        )
    }

    /// The room a branch is picked from is the one its plan names; the other
    /// axis has no say in it.
    #[kithara::test]
    fn the_room_picks_the_branch_by_its_own_axis() {
        let wide = Size::new(250.0, 10.0);

        assert_eq!(measured(MeasureAxis::Width).pick(wide), 2);
        assert_eq!(measured(MeasureAxis::Height).pick(wide), 0);
    }
}
