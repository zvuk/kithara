use crate::{
    expand::Binding,
    module::MeasureAxis,
    size::{SizeSpec, stands},
};

/// The band of room a cell stands in: from this much room on the measured
/// axis, and until that much. An open ceiling means it never goes away again.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct Band {
    pub until: Option<f32>,
    pub from: f32,
}

impl Band {
    /// A cell that names no band stands in every room its flow is given.
    pub const ALWAYS: Self = Self {
        from: 0.0,
        until: None,
    };

    #[must_use]
    pub const fn new(from: f32, until: Option<f32>) -> Self {
        Self { until, from }
    }

    /// Whether this cell stands in the room its flow turned out to have.
    #[must_use]
    pub fn stands(self, room: f32) -> bool {
        stands(self.from, self.until, room)
    }
}

/// One cell of a split, as its host mounts it.
#[non_exhaustive]
pub struct SplitMount<T> {
    /// The room this cell stands in.
    pub band: Band,
    /// What the document reads to know this cell is hidden, when the cell is a
    /// block at all. A host that mounts a hidden cell keeps the binding and
    /// reads it again; one that leaves it out never sees the cell.
    pub block: Option<Binding>,
    /// The box it composes to.
    pub size: SizeSpec,
    pub output: T,
    /// Its share of the room among the cells standing beside it.
    pub weight: f32,
}

/// One child of a row or column, as its host mounts it.
#[non_exhaustive]
pub struct GroupMount<T> {
    /// The room this child stands in.
    pub band: Band,
    /// What the document reads to know this child is hidden, when the child is
    /// a block at all.
    pub block: Option<Binding>,
    /// What it needs on the flow's own axis, when it names a floor.
    pub minimum: Option<f32>,
    pub output: T,
}

/// Branches whose choice belongs to the layout pass.
///
/// The document names a threshold per branch on one axis, and only the pass
/// that knows the room can say which branch stands. Every branch is mounted;
/// `steps` is one shorter than the branches, because the first stands below
/// the first threshold.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Measured {
    /// The axis whose room decides.
    pub axis: MeasureAxis,
    /// The box the node itself asks for.
    pub size: SizeSpec,
    /// The room each branch after the first stands from, in document order.
    pub steps: Vec<f32>,
}

impl Measured {
    /// Which branch stands in this much room, as an index into the branches.
    #[must_use]
    pub fn branch(&self, room: f32) -> usize {
        self.steps
            .iter()
            .rposition(|from| *from <= room)
            .map_or(0, |index| index + 1)
    }
}
