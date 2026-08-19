use iced::{
    Alignment, Element, Event, Length, Padding, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Clipboard, Shell, Widget as IcedWidget,
        layout::{self, Layout},
        mouse, overlay, renderer,
        widget::{self, Operation, Tree},
    },
};

use crate::{
    layout::Axis,
    render::UiEvent,
    solve::{self, Distribution, Input, Measure},
};

pub(super) struct Flex<'a> {
    axis: Axis,
    spacing: f32,
    padding: Padding,
    width: Length,
    height: Length,
    align: Alignment,
    children: Vec<Element<'a, UiEvent>>,
    child_layouts: Vec<ChildLayout>,
}

#[derive(Clone, Copy)]
struct ChildLayout {
    declared: Option<Size<Length>>,
    main_minimum: Option<f32>,
    main_weight: Option<f32>,
}

impl<'a> Flex<'a> {
    pub(super) fn row(
        children: impl IntoIterator<Item = (Element<'a, UiEvent>, Option<f32>)>,
    ) -> Self {
        Self::with_children(
            Axis::Horizontal,
            children
                .into_iter()
                .map(|(child, main_minimum)| (child, None, main_minimum, None)),
        )
    }

    pub(super) fn column(
        children: impl IntoIterator<Item = (Element<'a, UiEvent>, Option<f32>)>,
    ) -> Self {
        Self::with_children(
            Axis::Vertical,
            children
                .into_iter()
                .map(|(child, main_minimum)| (child, None, main_minimum, None)),
        )
    }

    pub(super) fn row_weighted(
        children: impl IntoIterator<Item = (Element<'a, UiEvent>, Size<Length>, f32)>,
    ) -> Self {
        Self::with_children(
            Axis::Horizontal,
            children.into_iter().map(|(child, declared, main_weight)| {
                (child, Some(declared), None, Some(main_weight))
            }),
        )
    }

    pub(super) fn column_weighted(
        children: impl IntoIterator<Item = (Element<'a, UiEvent>, Size<Length>, f32)>,
    ) -> Self {
        Self::with_children(
            Axis::Vertical,
            children.into_iter().map(|(child, declared, main_weight)| {
                (child, Some(declared), None, Some(main_weight))
            }),
        )
    }

    fn with_children(
        axis: Axis,
        children: impl IntoIterator<
            Item = (
                Element<'a, UiEvent>,
                Option<Size<Length>>,
                Option<f32>,
                Option<f32>,
            ),
        >,
    ) -> Self {
        let iterator = children.into_iter();
        let capacity = iterator.size_hint().0;
        let mut flex = Self {
            axis,
            spacing: 0.0,
            padding: Padding::ZERO,
            width: Length::Shrink,
            height: Length::Shrink,
            align: Alignment::Start,
            children: Vec::with_capacity(capacity),
            child_layouts: Vec::with_capacity(capacity),
        };

        for (child, declared, main_minimum, main_weight) in iterator {
            flex = flex.push(child, declared, main_minimum, main_weight);
        }

        flex
    }

    fn push(
        mut self,
        child: Element<'a, UiEvent>,
        declared: Option<Size<Length>>,
        main_minimum: Option<f32>,
        main_weight: Option<f32>,
    ) -> Self {
        let size_hint = declared.unwrap_or_else(|| child.as_widget().size_hint());

        if !size_hint.is_void() {
            self.width = self.width.enclose(size_hint.width);
            self.height = self.height.enclose(size_hint.height);
            self.children.push(child);
            self.child_layouts.push(ChildLayout {
                declared,
                main_minimum,
                main_weight,
            });
        }

        self
    }

    pub(super) fn spacing(mut self, spacing: f32) -> Self {
        self.spacing = spacing;
        self
    }

    pub(super) fn width(mut self, width: Length) -> Self {
        self.width = width;
        self
    }

    pub(super) fn height(mut self, height: Length) -> Self {
        self.height = height;
        self
    }

    pub(super) fn align(mut self, alignment: Alignment) -> Self {
        self.align = alignment;
        self
    }
}

impl IcedWidget<UiEvent, Theme, Renderer> for Flex<'_> {
    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn size_hint(&self) -> Size<Length> {
        Size::new(self.width, self.height)
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
        let solver_limits = limits.into();
        let items = self
            .children
            .iter()
            .zip(&self.child_layouts)
            .map(|(child, layout)| {
                let declared = layout.declared.unwrap_or_else(|| child.as_widget().size());
                layout.main_weight.map_or_else(
                    || solve::Item::new(declared.into(), layout.main_minimum),
                    |weight| solve::Item::weighted(declared.into(), weight),
                )
            })
            .collect::<Vec<_>>();
        let mut measure = IcedMeasure {
            children: &mut self.children,
            child_layouts: &self.child_layouts,
            trees: &mut tree.children,
            renderer,
            nodes: vec![layout::Node::default(); items.len()],
        };
        let Distribution {
            size,
            items: placements,
        } = solve::resolve(
            Input {
                axis: self.axis,
                limits: &solver_limits,
                width: self.width.into(),
                height: self.height.into(),
                padding: self.padding.into(),
                spacing: self.spacing,
                align_items: self.align.into(),
                items,
            },
            &mut measure,
        );
        let mut nodes = measure.nodes;

        for (node, placement) in nodes.iter_mut().zip(placements) {
            node.move_to_mut(placement.offset);
        }

        layout::Node::with_children(size.into(), nodes)
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
            for ((child, tree), layout) in self
                .children
                .iter()
                .zip(&tree.children)
                .zip(layout.children())
                .filter(|(_, layout)| layout.bounds().intersects(viewport))
            {
                child
                    .as_widget()
                    .draw(tree, renderer, theme, style, layout, cursor, viewport);
            }
        }
    }

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::stateless()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::None
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
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
        for ((child, tree), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                tree, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, tree), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(tree, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, UiEvent, Theme, Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
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
    children: &'a mut [Element<'element, UiEvent>],
    child_layouts: &'a [ChildLayout],
    trees: &'a mut [Tree],
    renderer: &'a Renderer,
    nodes: Vec<layout::Node>,
}

impl Measure for IcedMeasure<'_, '_> {
    fn measure(&mut self, index: usize, limits: &solve::Limits) -> solve::Size {
        let declared: Option<solve::Size<solve::Length>> =
            self.child_layouts[index].declared.map(Into::into);
        let child_limits =
            declared.map(|declared| limits.width(declared.width).height(declared.height).loose());
        let child_limits = child_limits.unwrap_or(*limits).into();
        let node = self.children[index].as_widget_mut().layout(
            &mut self.trees[index],
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
        self.nodes[index] = node;
        size
    }
}
