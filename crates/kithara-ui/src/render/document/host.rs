use super::{Group, Module, Popover};
use crate::{
    draw::Transform,
    expand::{Binding, ControlSpec, ExpandedNode},
    ids::InternId,
    layout::Axis,
    render::InputOwner,
    size::SizeSpec,
};

/// Mounts the toolkit-neutral document walk into one host's output tree.
///
/// The facade owns recursion and state-dependent document selection. A host
/// implements only the local layout, paint, and interaction mounting for each
/// node it receives.
pub trait Host {
    /// Complete host output produced for one document node.
    type Output;

    /// Mounts a weighted layout split.
    fn split(&mut self, axis: Axis, children: Vec<(f32, SizeSpec, Self::Output)>) -> Self::Output;

    /// Mounts one compiled module around its already-produced content.
    fn module(&mut self, module: Module<'_>, content: Option<Self::Output>) -> Self::Output;

    /// Mounts a row or column around its already-produced visible children.
    fn group(
        &mut self,
        group: Group<'_>,
        children: Vec<(Option<f32>, Self::Output)>,
    ) -> Self::Output;

    /// Mounts an anchored popover around its produced anchor and optional content.
    fn popover(
        &mut self,
        popover: Popover,
        anchor: Self::Output,
        content: Option<Self::Output>,
    ) -> Self::Output;

    /// Mounts a pressable document node.
    fn pressable(
        &mut self,
        path: InternId,
        child: Self::Output,
        size: Option<SizeSpec>,
    ) -> Self::Output;

    /// Mounts a bounded viewport over one taller subtree.
    ///
    /// The document declares the window, not the travel: how far the content
    /// may move, and what a wheel is worth, belong to the host that laid the
    /// child out, because only it knows how tall the child turned out to be.
    fn scroll(&mut self, id: InternId, child: Self::Output, size: Option<SizeSpec>)
    -> Self::Output;

    /// Mounts a vertical slot of visible children.
    fn slot(&mut self, children: Vec<Self::Output>, size: Option<SizeSpec>) -> Self::Output;

    /// Mounts children that all share one box, in document order.
    ///
    /// The host decides nothing about where they land: that is the business of
    /// whatever object wraps each of them, and a stage with no objects in it
    /// simply draws its children on top of one another.
    fn stage(&mut self, children: Vec<Self::Output>, size: Option<SizeSpec>) -> Self::Output;

    /// Mounts one compiled control leaf.
    ///
    /// `transform` is every enclosing object's pose folded into one offset,
    /// expressed in the box this control paints into. A host applies it to the
    /// neutral draw list rather than asking its toolkit to turn the widget:
    /// only one of the two toolkits can, and the two would disagree.
    fn control(
        &mut self,
        path: InternId,
        spec: &ControlSpec,
        read: Option<&Binding>,
        owner: InputOwner,
        size: Option<SizeSpec>,
        transform: Transform,
    ) -> Self::Output;

    /// Mounts the retained interaction owner around one produced subtree.
    fn hosted(&mut self, node: &ExpandedNode, child: Self::Output) -> Self::Output;

    /// Finishes the whole document with host-owned window layers.
    fn window(
        &mut self,
        content: Self::Output,
        dragged: Option<String>,
        resize_edges: bool,
    ) -> Self::Output;
}
