use iced::{
    Alignment, Element, Event, Length, Padding, Rectangle, Size, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget,
        layout::{self, Limits, Node as LayoutNode, flex},
        mouse::{Cursor, Interaction},
        overlay::{Element as OverlayElement, Group as OverlayGroup},
        renderer,
        widget::{
            Tree,
            tree::{self, State as TreeState, Tag as TreeTag},
        },
    },
};

use crate::{layout::Axis, module::MeasureAxis, size::stands};

/// A child and the band of room it stands in, as `(from, until)`.
pub(crate) type Cell<'a, Message, Theme, Renderer> =
    ((f32, Option<f32>), Element<'a, Message, Theme, Renderer>);

/// Lays out the children whose band the box it is given falls in. A child
/// outside its band takes neither its own room nor a gap beside it, and the
/// box the widget answers is the one it declares either way.
pub(crate) struct Revealed<'a, Message, Theme = iced::Theme, Renderer = iced::Renderer> {
    children: Vec<Element<'a, Message, Theme, Renderer>>,
    /// One band per child, in logical pixels of the measured axis.
    bands: Vec<(f32, Option<f32>)>,
    shape: Shape,
}

/// What the container declares about its own box: the axis it lays children
/// out along, the axis it measures, the box it answers, and how it spaces and
/// aligns what it shows.
pub(crate) struct Shape {
    pub(crate) flow: Axis,
    pub(crate) measure: MeasureAxis,
    pub(crate) size: Size<Length>,
    pub(crate) padding: Padding,
    pub(crate) gap: f32,
    pub(crate) align: Alignment,
}

/// Which children the last layout pass revealed, so drawing and events reach
/// the ones that were laid out.
#[derive(Default)]
struct State {
    shown: Vec<bool>,
}

/// Rotations that bring the shown children to the front of a slice without
/// disturbing their order. Replayed backwards they put the slice back.
fn gather(shown: &[bool]) -> Vec<(usize, usize)> {
    shown
        .iter()
        .enumerate()
        .filter_map(|(index, on)| on.then_some(index))
        .enumerate()
        .filter(|(front, index)| index > front)
        .collect()
}

impl<'a, Message, Theme, Renderer> Revealed<'a, Message, Theme, Renderer> {
    /// Each child carries the band it stands in, so the two lists cannot drift
    /// apart.
    pub(crate) fn new(children: Vec<Cell<'a, Message, Theme, Renderer>>, shape: Shape) -> Self {
        let (bands, children) = children.into_iter().unzip();
        Self {
            children,
            bands,
            shape,
        }
    }

    fn reached(&self, room: Size) -> Vec<bool> {
        let value = match self.shape.measure {
            MeasureAxis::Width => room.width,
            MeasureAxis::Height => room.height,
        };
        self.bands
            .iter()
            .map(|(from, until)| stands(*from, *until, value))
            .collect()
    }

    fn flow(&self) -> flex::Axis {
        match self.shape.flow {
            Axis::Horizontal => flex::Axis::Horizontal,
            Axis::Vertical => flex::Axis::Vertical,
        }
    }

    /// The children the last pass revealed, each with its state and its box.
    fn revealed<'t>(
        &'t self,
        tree: &'t Tree,
        layout: Layout<'t>,
    ) -> impl Iterator<
        Item = (
            &'t Element<'a, Message, Theme, Renderer>,
            &'t Tree,
            Layout<'t>,
        ),
    > {
        let shown = tree.state.downcast_ref::<State>().shown.iter();
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .zip(shown)
            .filter_map(|(((child, state), bounds), on)| on.then_some((child, state, bounds)))
    }

    fn revealed_mut<'t>(
        &'t mut self,
        tree: &'t mut Tree,
        layout: Layout<'t>,
    ) -> impl Iterator<
        Item = (
            &'t mut Element<'a, Message, Theme, Renderer>,
            &'t mut Tree,
            Layout<'t>,
        ),
    > {
        let Tree {
            state, children, ..
        } = tree;
        let shown = state.downcast_ref::<State>().shown.iter();
        self.children
            .iter_mut()
            .zip(children)
            .zip(layout.children())
            .zip(shown)
            .filter_map(|(((child, state), bounds), on)| on.then_some((child, state, bounds)))
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Revealed<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        self.shape.size
    }

    fn state(&self) -> tree::State {
        TreeState::new(State::default())
    }

    fn tag(&self) -> tree::Tag {
        TreeTag::of::<State>()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> layout::Node {
        let declared = limits
            .width(self.shape.size.width)
            .height(self.shape.size.height);
        let shown = self.reached(declared.max());
        let moves = gather(&shown);
        for (front, index) in moves.iter().copied() {
            self.children[front..=index].rotate_right(1);
            tree.children[front..=index].rotate_right(1);
        }
        let count = shown.iter().filter(|on| **on).count();
        let node = flex::resolve(
            self.flow(),
            renderer,
            limits,
            self.shape.size.width,
            self.shape.size.height,
            self.shape.padding,
            self.shape.gap,
            self.shape.align,
            &mut self.children[..count],
            &mut tree.children[..count],
        );
        for (front, index) in moves.iter().rev().copied() {
            self.children[front..=index].rotate_left(1);
            tree.children[front..=index].rotate_left(1);
        }

        let mut nodes = vec![LayoutNode::default(); self.children.len()];
        let slots = shown
            .iter()
            .enumerate()
            .filter_map(|(index, on)| on.then_some(index));
        slots
            .zip(node.children())
            .for_each(|(slot, laid_out)| nodes[slot] = laid_out.clone());
        tree.state.downcast_mut::<State>().shown = shown;
        LayoutNode::with_children(node.size(), nodes)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        for (child, state, bounds) in self.revealed(tree, layout) {
            child
                .as_widget()
                .draw(state, renderer, theme, style, bounds, cursor, viewport);
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> Interaction {
        self.revealed(tree, layout)
            .map(|(child, state, bounds)| {
                child
                    .as_widget()
                    .mouse_interaction(state, bounds, cursor, viewport, renderer)
            })
            .fold(Interaction::default(), Ord::max)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        for (child, state, bounds) in self.revealed_mut(tree, layout) {
            child.as_widget_mut().update(
                state, event, bounds, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<OverlayElement<'b, Message, Theme, Renderer>> {
        let floating: Vec<_> = self
            .revealed_mut(tree, layout)
            .filter_map(|(child, state, bounds)| {
                child
                    .as_widget_mut()
                    .overlay(state, bounds, renderer, viewport, translation)
            })
            .collect();
        (!floating.is_empty()).then(|| OverlayGroup::with_children(floating).overlay())
    }
}

impl<'a, Message, Theme, Renderer> From<Revealed<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(revealed: Revealed<'a, Message, Theme, Renderer>) -> Self {
        Self::new(revealed)
    }
}

#[cfg(test)]
mod tests {
    use iced::{
        Alignment, Element, Length, Padding, Size, Theme,
        advanced::{
            Widget,
            layout::Limits,
            widget::{Tree, tree::Tag},
        },
        widget::{Row, Space},
    };
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{layout::Axis, module::MeasureAxis, render::UiEvent};

    const GAP: f32 = 4.0;

    fn cell(width: f32) -> Element<'static, UiEvent, Theme, ()> {
        Space::new().width(width).height(20.0).into()
    }

    /// A bar whose cells appear at thresholds of their own: the middle one
    /// last, so a room that reveals two of three leaves a hole in the middle.
    fn bar(padding: Padding) -> Revealed<'static, UiEvent, Theme, ()> {
        Revealed::new(
            vec![
                ((0.0, None), cell(10.0)),
                ((440.0, None), cell(20.0)),
                ((350.0, None), cell(30.0)),
            ],
            Shape {
                flow: Axis::Horizontal,
                measure: MeasureAxis::Width,
                size: Size::new(Length::Fill, Length::Fixed(42.0)),
                padding,
                gap: GAP,
                align: Alignment::Center,
            },
        )
    }

    fn tree_for(widget: &Revealed<'_, UiEvent, Theme, ()>) -> Tree {
        Tree {
            tag: Tag::of::<State>(),
            state: Widget::<UiEvent, Theme, ()>::state(widget),
            children: Widget::<UiEvent, Theme, ()>::children(widget),
        }
    }

    /// The widths and offsets of every child box, zero standing for a child
    /// the room left out.
    fn lay_out(
        widget: &mut Revealed<'_, UiEvent, Theme, ()>,
        tree: &mut Tree,
        room: f32,
    ) -> (Size, Vec<(f32, f32)>) {
        let limits = Limits::new(Size::ZERO, Size::new(room, 42.0));
        let node = widget.layout(tree, &(), &limits);
        let boxes = node
            .children()
            .iter()
            .map(|child| (child.bounds().x, child.bounds().width))
            .collect();
        (node.size(), boxes)
    }

    /// The share of the room a layout split gives one cell, which is what
    /// `split_length` hands the widget.
    fn portion(weight: u16) -> Element<'static, UiEvent, Theme, ()> {
        Space::new()
            .width(Length::FillPortion(weight))
            .height(Length::Fill)
            .into()
    }

    fn boxes(node: &layout::Node) -> Vec<(f32, f32)> {
        node.children()
            .iter()
            .map(|child| (child.bounds().x, child.bounds().width))
            .collect()
    }

    /// A measuring split lays its cells out through the same resolver a plain
    /// one does, so the weights the document names divide the room the same
    /// way and the cell a band leaves out takes none of it.
    #[kithara::test]
    fn a_measuring_row_gives_its_cells_the_portions_a_plain_row_gives() {
        let room = Limits::new(Size::ZERO, Size::new(400.0, 42.0));
        let mut widget = Revealed::new(
            vec![
                ((0.0, None), portion(1)),
                ((0.0, None), portion(3)),
                ((900.0, None), portion(4)),
            ],
            Shape {
                flow: Axis::Horizontal,
                measure: MeasureAxis::Width,
                size: Size::new(Length::Fill, Length::Fixed(42.0)),
                padding: Padding::ZERO,
                gap: 0.0,
                align: Alignment::Start,
            },
        );
        let mut tree = tree_for(&widget);
        let mut plain: Element<'_, UiEvent, Theme, ()> =
            Row::with_children(vec![portion(1), portion(3)])
                .width(Length::Fill)
                .height(Length::Fixed(42.0))
                .into();
        let mut plain_tree = Tree::new(&plain);

        let revealed = boxes(&widget.layout(&mut tree, &(), &room));
        let laid_out = boxes(&plain.as_widget_mut().layout(&mut plain_tree, &(), &room));

        assert_eq!(laid_out, vec![(0.0, 100.0), (100.0, 300.0)]);
        assert_eq!(
            revealed,
            vec![(0.0, 100.0), (100.0, 300.0), (0.0, 0.0)],
            "the cell waiting for a room the bar does not have takes none of it",
        );
    }

    #[kithara::test]
    fn a_cell_appears_at_its_own_threshold() {
        let mut widget = bar(Padding::ZERO);
        let mut tree = tree_for(&widget);

        let (_, narrow) = lay_out(&mut widget, &mut tree, 300.0);
        let (_, wide) = lay_out(&mut widget, &mut tree, 500.0);

        assert_eq!(narrow, vec![(0.0, 10.0), (0.0, 0.0), (0.0, 0.0)]);
        assert_eq!(wide, vec![(0.0, 10.0), (14.0, 20.0), (38.0, 30.0)]);
    }

    /// The cell left out costs neither its width nor the gap beside it, so its
    /// neighbours close up as if it were never declared.
    #[kithara::test]
    fn a_hidden_cell_charges_no_gap() {
        let mut widget = bar(Padding::ZERO);
        let mut tree = tree_for(&widget);

        let (_, boxes) = lay_out(&mut widget, &mut tree, 400.0);

        assert_eq!(boxes, vec![(0.0, 10.0), (0.0, 0.0), (14.0, 30.0)]);
    }

    /// Laying out the revealed cells reorders nothing that outlives the pass:
    /// a widget's state is its position, so the next pass would address the
    /// wrong cell.
    #[kithara::test]
    fn a_pass_that_reveals_some_cells_leaves_the_order_it_found() {
        let mut widget = bar(Padding::ZERO);
        let mut tree = tree_for(&widget);

        lay_out(&mut widget, &mut tree, 400.0);
        let (_, boxes) = lay_out(&mut widget, &mut tree, 500.0);

        assert_eq!(boxes, vec![(0.0, 10.0), (14.0, 20.0), (38.0, 30.0)]);
    }

    /// A band ends where the next one begins, so the two cells sharing that
    /// number never both stand and never both go missing.
    #[kithara::test]
    fn a_band_hands_the_line_over_at_the_number_it_ends_on() {
        let mut widget = Revealed::new(
            vec![
                ((0.0, Some(350.0)), cell(10.0)),
                ((350.0, None), cell(30.0)),
            ],
            Shape {
                flow: Axis::Horizontal,
                measure: MeasureAxis::Width,
                size: Size::new(Length::Fill, Length::Fixed(42.0)),
                padding: Padding::ZERO,
                gap: GAP,
                align: Alignment::Center,
            },
        );
        let mut tree = tree_for(&widget);

        let (_, below) = lay_out(&mut widget, &mut tree, 349.0);
        let (_, reached) = lay_out(&mut widget, &mut tree, 350.0);

        assert_eq!(
            below,
            vec![(0.0, 10.0), (0.0, 0.0)],
            "349 is still the strip"
        );
        assert_eq!(reached, vec![(0.0, 0.0), (0.0, 30.0)], "350 is the wave");
    }

    /// The container measures the box it declares, padding included, so a
    /// threshold is the number the parent hands it.
    #[kithara::test]
    fn a_threshold_is_read_against_the_declared_box() {
        let mut widget = bar(Padding::ZERO.left(30.0).right(30.0));
        let mut tree = tree_for(&widget);

        let (size, boxes) = lay_out(&mut widget, &mut tree, 360.0);

        assert_eq!(size, Size::new(360.0, 42.0), "the box it answers");
        assert_eq!(boxes[2].1, 30.0, "360 reaches 350 before padding is spent");
    }
}
