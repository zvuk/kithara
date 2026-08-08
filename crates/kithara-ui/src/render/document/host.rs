use super::{Group, Module, Popover};
use crate::{
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

    /// Mounts a vertical slot of visible children.
    fn slot(&mut self, children: Vec<Self::Output>, size: Option<SizeSpec>) -> Self::Output;

    /// Mounts one compiled control leaf.
    fn control(
        &mut self,
        path: InternId,
        spec: &ControlSpec,
        read: Option<&Binding>,
        owner: InputOwner,
        size: Option<SizeSpec>,
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
