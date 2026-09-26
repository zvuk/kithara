#[cfg(test)]
mod tests;
mod tree;

pub use self::tree::{
    Binding, BindingKind, BlockSpec, ControlSpec, DropSpec, ExpandedNode, MagnetSpec, MeasureSpec,
    SurfaceSpec,
};
pub(crate) use self::tree::{
    Budget, ControlSite, ControlVisitor, ExpandedInclude, ExpandedModule, Unprompted,
    adaptive_branch, motion_of,
};
