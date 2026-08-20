use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme,
    advanced::{
        Clipboard, Layout, Shell, Widget,
        layout::{self, Limits, Node as LayoutNode},
        mouse::{Cursor, Interaction},
        overlay::Element as OverlayElement,
        renderer,
        widget::{
            Tree,
            tree::{self, State as TreeState, Tag as TreeTag},
        },
    },
};

use crate::module::MeasureAxis;

/// Draws the branch that fits the box the toolkit gives it. Every branch is
/// built once and kept, so switching costs no rebuild and each branch keeps the
/// state it had.
pub(crate) struct Measured<'a, Message> {
    branches: Vec<Element<'a, Message>>,
    /// One threshold per branch after the first, ascending.
    steps: Vec<f32>,
    axis: MeasureAxis,
    size: Size<Length>,
}

/// Which branch the last layout pass picked, so drawing and events reach the
/// one that was laid out.
#[derive(Default)]
struct State {
    drawn: usize,
}

impl<'a, Message> Measured<'a, Message> {
    pub(crate) fn new(
        branches: Vec<Element<'a, Message>>,
        steps: Vec<f32>,
        axis: MeasureAxis,
        size: Size<Length>,
    ) -> Self {
        Self {
            branches,
            steps,
            axis,
            size,
        }
    }

    fn pick(&self, room: Size) -> usize {
        let value = match self.axis {
            MeasureAxis::Width => room.width,
            MeasureAxis::Height => room.height,
        };
        self.steps
            .iter()
            .rposition(|from| *from <= value)
            .map_or(0, |index| index + 1)
    }

    fn drawn(tree: &Tree) -> usize {
        tree.state.downcast_ref::<State>().drawn
    }
}

impl<Message> Widget<Message, Theme, Renderer> for Measured<'_, Message> {
    fn children(&self) -> Vec<Tree> {
        self.branches.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.branches);
    }

    fn size(&self) -> Size<Length> {
        self.size
    }

    fn state(&self) -> tree::State {
        TreeState::new(State::default())
    }

    fn tag(&self) -> tree::Tag {
        TreeTag::of::<State>()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> layout::Node {
        let limits = limits.width(self.size.width).height(self.size.height);
        let drawn = self.pick(limits.max());
        tree.state.downcast_mut::<State>().drawn = drawn;

        let mut nodes: Vec<_> = self
            .branches
            .iter()
            .map(|_| LayoutNode::default())
            .collect();
        let node = self.branches[drawn].as_widget_mut().layout(
            &mut tree.children[drawn],
            renderer,
            &limits,
        );
        let size = limits.resolve(self.size.width, self.size.height, node.size());
        nodes[drawn] = node;
        LayoutNode::with_children(size, nodes)
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
        let drawn = Self::drawn(tree);
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

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> Interaction {
        let drawn = Self::drawn(tree);
        layout
            .children()
            .nth(drawn)
            .map_or_else(Interaction::default, |bounds| {
                self.branches[drawn].as_widget().mouse_interaction(
                    &tree.children[drawn],
                    bounds,
                    cursor,
                    viewport,
                    renderer,
                )
            })
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
        let drawn = Self::drawn(tree);
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

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: iced::Vector,
    ) -> Option<OverlayElement<'b, Message, Theme, Renderer>> {
        let drawn = Self::drawn(tree);
        let bounds = layout.children().nth(drawn)?;
        self.branches[drawn].as_widget_mut().overlay(
            &mut tree.children[drawn],
            bounds,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message> From<Measured<'a, Message>> for Element<'a, Message>
where
    Message: 'a,
{
    fn from(measured: Measured<'a, Message>) -> Self {
        Self::new(measured)
    }
}

#[cfg(test)]
mod tests {
    use iced::widget::Space;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::render::UiEvent;

    fn measured(axis: MeasureAxis) -> Measured<'static, UiEvent> {
        let branch = || Element::<UiEvent>::from(Space::new());
        Measured::new(
            vec![branch(), branch(), branch()],
            vec![100.0, 200.0],
            axis,
            Size::new(Length::Fill, Length::Fill),
        )
    }

    #[kithara::test]
    fn the_room_picks_the_branch_by_its_own_axis() {
        let wide = Size::new(250.0, 10.0);

        assert_eq!(measured(MeasureAxis::Width).pick(wide), 2);
        assert_eq!(measured(MeasureAxis::Height).pick(wide), 0);
    }

    #[kithara::test]
    fn a_threshold_is_reached_at_its_own_value() {
        let node = measured(MeasureAxis::Width);

        assert_eq!(node.pick(Size::new(99.0, 0.0)), 0);
        assert_eq!(node.pick(Size::new(100.0, 0.0)), 1);
        assert_eq!(node.pick(Size::new(199.0, 0.0)), 1);
        assert_eq!(node.pick(Size::new(200.0, 0.0)), 2);
    }

    /// An unbounded axis is what a parent says when it will take whatever the
    /// content asks for, and the widest branch is what fits that.
    #[kithara::test]
    fn an_unbounded_axis_takes_the_last_branch() {
        assert_eq!(
            measured(MeasureAxis::Width).pick(Size::new(f32::INFINITY, 0.0)),
            2
        );
    }
}
