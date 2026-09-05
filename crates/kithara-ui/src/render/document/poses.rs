use std::collections::BTreeMap;

use super::{
    Ctx, Group, GroupMount, Host, Measured, Module, PlacedMount, Popover, SplitMount, render,
};
use crate::{
    compile::CompiledNode,
    draw::Transform,
    expand::{Binding, ControlSpec, ExpandedNode},
    ids::InternId,
    layout::Axis,
    module::MeasureAxis,
    render::InputOwner,
    size::SizeSpec,
};

/// Where this frame puts every control the document shows.
///
/// A host that rebuilds its output each frame learns this from the mount
/// itself. One that keeps a tree between frames does not: it re-reads the
/// endpoints into widgets it already has, and an object's pose is not any
/// widget's endpoint — it is worked out by the document walk. So the walk is
/// run again, through a host that mounts nothing, and the answers are pushed
/// into the tree that is already standing.
///
/// This is the same walk and the same [`super::facade`] arithmetic the mount
/// used, not a second implementation of it, which is the whole reason it is a
/// [`Host`] rather than a bespoke traversal.
#[must_use]
pub(crate) fn placements(node: &CompiledNode, ctx: Ctx<'_, '_>) -> BTreeMap<InternId, Transform> {
    let mut poses = Poses {
        placed: BTreeMap::new(),
    };
    let () = render(node, ctx, &mut poses);
    poses.placed
}

struct Poses {
    placed: BTreeMap<InternId, Transform>,
}

impl Host for &mut Poses {
    type Output = ();

    fn control(
        &mut self,
        path: InternId,
        _spec: &ControlSpec,
        _read: Option<&Binding>,
        _owner: InputOwner,
        _size: Option<SizeSpec>,
        transform: Transform,
    ) {
        self.placed.insert(path, transform);
    }

    fn group(&mut self, _group: Group<'_>, _children: Vec<GroupMount<Self::Output>>) {}

    fn hosted(&mut self, _node: &ExpandedNode, _child: Self::Output) {}

    fn measured(&mut self, _plan: Measured, _branches: Vec<Self::Output>) {}

    fn module(&mut self, _module: Module<'_>, _content: Option<Self::Output>) {}

    /// A placement is laid out where its point puts it rather than offset
    /// from where it stands, so there is no pose here to collect.
    fn placed(&mut self, _placement: PlacedMount<'_>, _child: Self::Output) {}

    /// Poses only what the document shows. A host that mounts a shut surface
    /// anyway keeps its content in the tree, but placing objects nobody can see
    /// would be work on every refresh for a menu that is standing aside.
    fn popover(
        &mut self,
        popover: Popover<'_>,
        _anchor: Self::Output,
        content: &mut dyn FnMut(&mut Self) -> Self::Output,
    ) {
        if popover.is_open() {
            content(self);
        }
    }

    fn pressable(&mut self, _path: InternId, _child: Self::Output, _size: Option<SizeSpec>) {}

    fn scroll(&mut self, _id: InternId, _child: Self::Output, _size: Option<SizeSpec>) {}

    fn slot(&mut self, _children: Vec<GroupMount<Self::Output>>, _size: Option<SizeSpec>) {}

    fn split(
        &mut self,
        _axis: Axis,
        _measure: Option<MeasureAxis>,
        _children: Vec<SplitMount<Self::Output>>,
    ) {
    }

    fn stage(&mut self, _children: Vec<Self::Output>, _size: Option<SizeSpec>) {}

    fn window(&mut self, _content: Self::Output, _carried: Option<&Binding>, _resize_edges: bool) {}
}
