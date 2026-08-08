/// Who answers pointer input for the subtree a node renders into: the leaf
/// itself, or the retained engine hosting it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InputOwner {
    /// Each mounted control owns its toolkit-local input state.
    Leaf,
    /// The retained document engine owns input for the mounted subtree.
    Engine,
}
