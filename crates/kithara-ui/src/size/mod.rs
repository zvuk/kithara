mod spec;
#[cfg(test)]
mod tests;

pub(crate) use self::spec::{
    BlockNode, Cell, Cells, DEFAULTS, NOTHING, Snapshot, at_least, axis_dim, axis_min, branch,
    combine_horizontal, combine_vertical, compiled_node_size_with_hidden, compute_size,
    effective_size, has_blocks, is_hidden, min_size, rooms, settled, stands,
    visible_compiled_children, with_module_chrome,
};
pub use self::spec::{Dim, SizeSpec, control_size};
