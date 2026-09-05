use super::{Group, GroupMount, Measured, Module, PlacedMount, Popover, SplitMount};
use crate::{
    draw::Transform,
    expand::{Binding, ControlSpec, ExpandedNode},
    ids::InternId,
    layout::Axis,
    module::MeasureAxis,
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

    /// Whether a block the document hides is mounted anyway.
    ///
    /// The reason is the one a shut popover is mounted for: a host that
    /// rebuilds its tree every frame leaves a hidden block out and pays
    /// nothing, while one that mounts a tree and keeps it has to mount the
    /// block while it is hidden, because a block missing from the tree could
    /// never come back without rebuilding everything around it. A flow tells
    /// such a host which of its children are blocks, and it hides them the way
    /// it hides a child the room did not reach.
    const MOUNTS_HIDDEN: bool = false;

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

    /// Mounts a row or column around its already-produced visible children.
    ///
    /// A group that names a `measure` shows only the children whose band holds
    /// the room it turned out to have; the rest are mounted and stand aside.
    fn group(&mut self, group: Group<'_>, children: Vec<GroupMount<Self::Output>>) -> Self::Output;

    /// Mounts the retained interaction owner around one produced subtree.
    fn hosted(&mut self, node: &ExpandedNode, child: Self::Output) -> Self::Output;

    /// Mounts the branches of a node that draws whichever one fits its room.
    ///
    /// Every branch is mounted because the choice is the layout pass's to make;
    /// the host draws, measures, and drives only the one that stands.
    fn measured(&mut self, plan: Measured, branches: Vec<Self::Output>) -> Self::Output;

    /// Mounts one compiled module around its already-produced content.
    fn module(&mut self, module: Module<'_>, content: Option<Self::Output>) -> Self::Output;

    /// Mounts one placement of a stage around the subtree it holds.
    ///
    /// The host puts that subtree at the placement's point and, where the
    /// placement has somewhere to write, lets the pointer carry it: what a
    /// drag publishes is [`Snap::take`] of where it was left, so the magnet
    /// answers the same in both hosts.
    fn placed(&mut self, placement: PlacedMount<'_>, child: Self::Output) -> Self::Output;

    /// Mounts an anchored popover around its produced anchor, and around the
    /// content it expands from `content` if it wants it.
    ///
    /// The content is handed over unexpanded because the two kinds of host want
    /// opposite things from a closed surface. One that rebuilds its tree every
    /// frame gains nothing by producing content nobody sees, and pays for every
    /// endpoint read below it. One that mounts a tree and keeps it has to mount
    /// the content while it is shut, because a surface missing from the tree
    /// could never be opened without rebuilding everything around it.
    fn popover(
        &mut self,
        popover: Popover<'_>,
        anchor: Self::Output,
        content: &mut dyn FnMut(&mut Self) -> Self::Output,
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

    /// Mounts a vertical slot around its already-produced children.
    ///
    /// A slot is a flow like any other, so it is handed every child this host
    /// mounts and stands the ones the document does not hide - the block a
    /// host keeps in its tree is the host's to hide, not the facade's to drop.
    fn slot(
        &mut self,
        children: Vec<GroupMount<Self::Output>>,
        size: Option<SizeSpec>,
    ) -> Self::Output;

    /// Mounts a weighted layout split.
    ///
    /// A split that names a `measure` shows only the cells whose band holds the
    /// room it turned out to have, which is a question only the layout pass can
    /// answer: every cell is mounted either way.
    fn split(
        &mut self,
        axis: Axis,
        measure: Option<MeasureAxis>,
        children: Vec<SplitMount<Self::Output>>,
    ) -> Self::Output;

    /// Mounts children that all share one box, in document order.
    ///
    /// The host decides nothing about where they land: that is the business of
    /// whatever object wraps each of them, and a stage with no objects in it
    /// simply draws its children on top of one another.
    fn stage(&mut self, children: Vec<Self::Output>, size: Option<SizeSpec>) -> Self::Output;

    /// Finishes the whole document with host-owned window layers.
    ///
    /// `carried` is the binding the window names for what the pointer carries,
    /// not the reading of it, because the two kinds of host read it at
    /// different moments: one asks afresh every frame, and one mounts a layer
    /// for the life of the window and asks again whenever the document is
    /// shown.
    fn window(
        &mut self,
        content: Self::Output,
        carried: Option<&Binding>,
        resize_edges: bool,
    ) -> Self::Output;
}
