use std::collections::BTreeMap;

use super::{Ctx, Group, Host, Module, Popover, render};
use crate::{
    compile::CompiledNode,
    draw::Transform,
    expand::{Binding, ControlSpec, ExpandedNode},
    ids::InternId,
    layout::Axis,
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
    // This host mounts nothing, so the walk's own output is empty and what it
    // collected on the way is the answer.
    let () = render(node, ctx, &mut poses);
    poses.placed
}

struct Poses {
    placed: BTreeMap<InternId, Transform>,
}

impl Host for &mut Poses {
    type Output = ();

    fn split(&mut self, _axis: Axis, _children: Vec<(f32, SizeSpec, Self::Output)>) {}

    fn module(&mut self, _module: Module<'_>, _content: Option<Self::Output>) {}

    fn group(&mut self, _group: Group<'_>, _children: Vec<(Option<f32>, Self::Output)>) {}

    fn popover(
        &mut self,
        _popover: Popover,
        _anchor: Self::Output,
        _content: Option<Self::Output>,
    ) {
    }

    fn pressable(&mut self, _path: InternId, _child: Self::Output, _size: Option<SizeSpec>) {}

    fn scroll(&mut self, _id: InternId, _child: Self::Output, _size: Option<SizeSpec>) {}

    fn slot(&mut self, _children: Vec<Self::Output>, _size: Option<SizeSpec>) {}

    fn stage(&mut self, _children: Vec<Self::Output>, _size: Option<SizeSpec>) {}

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

    fn hosted(&mut self, _node: &ExpandedNode, _child: Self::Output) {}

    fn window(&mut self, _content: Self::Output, _dragged: Option<String>, _resize_edges: bool) {}
}
