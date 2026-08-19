mod binding_subst;
mod kind;
mod machine;
mod node;
mod site;
mod spec;
mod structural;

pub use binding_subst::scoped_key;
pub(crate) use binding_subst::{intern_binding, substitute_binding, substitute_map};
pub(crate) use machine::Expander;
pub use node::{Binding, BindingKind, BlockSpec, ControlSpec, DropSpec, ExpandedNode, SurfaceSpec};
pub(crate) use node::{
    Budget, ControlSite, ControlVisitor, ExpandedInclude, ExpandedModule, Unprompted, motion_of,
};

pub use crate::shader::ShaderSpec;
