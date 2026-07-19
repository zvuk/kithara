use serde::{Deserialize, Serialize};

use crate::{expand::ExpandedNode, registry::ControlCatalog};

/// One-axis size rule. `Fill` takes available space and has no intrinsic size.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub enum Dim {
    Fixed(f32),
    Range { min: f32, max: Option<f32> },
    Fill,
}

impl Dim {
    /// Returns the lower bound in logical pixels, or zero for [`Dim::Fill`].
    #[must_use]
    pub fn min(self) -> f32 {
        Bounds::from(self).min
    }

    /// Returns the upper bound, or `None` for an open range or [`Dim::Fill`].
    #[must_use]
    pub fn max(self) -> Option<f32> {
        Bounds::from(self).max
    }
}

/// Intrinsic size of a control or module on both axes.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SizeSpec {
    pub w: Dim,
    pub h: Dim,
}

impl SizeSpec {
    pub const FILL: Self = Self {
        w: Dim::Fill,
        h: Dim::Fill,
    };

    #[must_use]
    pub const fn new(w: Dim, h: Dim) -> Self {
        Self { w, h }
    }
}

#[derive(Clone, Copy)]
struct Bounds {
    min: f32,
    max: Option<f32>,
}

impl Bounds {
    const ZERO: Self = Self {
        min: 0.0,
        max: Some(0.0),
    };

    fn sum(self, dim: Dim) -> Self {
        Self {
            min: self.min + dim.min(),
            max: self.max.zip(dim.max()).map(|(left, right)| left + right),
        }
    }

    fn max(self, dim: Dim) -> Self {
        Self {
            min: self.min.max(dim.min()),
            max: self.max.zip(dim.max()).map(|(left, right)| left.max(right)),
        }
    }
}

impl From<Dim> for Bounds {
    fn from(dim: Dim) -> Self {
        match dim {
            Dim::Fixed(value) => Self {
                min: value,
                max: Some(value),
            },
            Dim::Range { min, max } => Self { min, max },
            Dim::Fill => Self {
                min: 0.0,
                max: None,
            },
        }
    }
}

impl From<Bounds> for Dim {
    fn from(bounds: Bounds) -> Self {
        match bounds.max {
            Some(max) if bounds.min.to_bits() == max.to_bits() => Self::Fixed(bounds.min),
            Some(max) => Self::Range {
                min: bounds.min,
                max: Some(max),
            },
            None if bounds.min.to_bits() == 0.0f32.to_bits() => Self::Fill,
            None => Self::Range {
                min: bounds.min,
                max: None,
            },
        }
    }
}

pub(crate) fn combine_horizontal(sizes: impl IntoIterator<Item = SizeSpec>) -> SizeSpec {
    let (width, height) = sizes
        .into_iter()
        .fold((Bounds::ZERO, Bounds::ZERO), |(width, height), size| {
            (width.sum(size.w), height.max(size.h))
        });
    SizeSpec::new(Dim::from(width), Dim::from(height))
}

pub(crate) fn combine_vertical(sizes: impl IntoIterator<Item = SizeSpec>) -> SizeSpec {
    let (width, height) = sizes
        .into_iter()
        .fold((Bounds::ZERO, Bounds::ZERO), |(width, height), size| {
            (width.max(size.w), height.sum(size.h))
        });
    SizeSpec::new(Dim::from(width), Dim::from(height))
}

/// Computes a node's intrinsic size from its override, children, or catalog entry.
#[must_use]
pub fn compute_size(node: &ExpandedNode, catalog: &dyn ControlCatalog) -> SizeSpec {
    let override_size = match node {
        ExpandedNode::Row { size, .. }
        | ExpandedNode::Column { size, .. }
        | ExpandedNode::Slot { size, .. }
        | ExpandedNode::Control { size, .. } => *size,
    };
    if let Some(size) = override_size {
        return size;
    }

    match node {
        ExpandedNode::Row { children, .. } => {
            combine_horizontal(children.iter().map(|child| compute_size(child, catalog)))
        }
        ExpandedNode::Column { children, .. } => {
            combine_vertical(children.iter().map(|child| compute_size(child, catalog)))
        }
        ExpandedNode::Slot { children, .. } if children.is_empty() => SizeSpec::FILL,
        ExpandedNode::Slot { children, .. } => {
            combine_vertical(children.iter().map(|child| compute_size(child, catalog)))
        }
        ExpandedNode::Control { kind, .. } => catalog
            .kind(kind)
            .map_or(SizeSpec::FILL, |description| description.size),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        ids::{ControlKind, NodeId},
        module::AdaptivePolicy,
        registry::ControlKindDesc,
    };

    struct EmptyCatalog;

    impl ControlCatalog for EmptyCatalog {
        fn kind(&self, _kind: &ControlKind) -> Option<&ControlKindDesc> {
            None
        }
    }

    fn control(id: &str, size: SizeSpec) -> ExpandedNode {
        ExpandedNode::Control {
            path: id.to_owned(),
            id: NodeId(id.to_owned()),
            kind: ControlKind("test".to_owned()),
            props: BTreeMap::new(),
            read: None,
            write: None,
            adaptive: AdaptivePolicy::default(),
            size: Some(size),
        }
    }

    fn fixed(w: f32, h: f32) -> SizeSpec {
        SizeSpec::new(Dim::Fixed(w), Dim::Fixed(h))
    }

    #[kithara::test]
    fn range_reports_its_bounds() {
        let dim = Dim::Range {
            min: 10.0,
            max: Some(20.0),
        };

        assert_eq!(dim.min(), 10.0);
        assert_eq!(dim.max(), Some(20.0));
    }

    #[kithara::test]
    fn fill_has_no_intrinsic_bounds() {
        assert_eq!(Dim::Fill.min(), 0.0);
        assert_eq!(Dim::Fill.max(), None);
    }

    #[kithara::test]
    fn row_sums_width_and_maximizes_height() {
        let node = ExpandedNode::Row {
            id: None,
            children: vec![
                control("left", fixed(10.0, 4.0)),
                control("right", fixed(6.0, 8.0)),
            ],
            size: None,
        };

        let size = compute_size(&node, &EmptyCatalog);

        assert_eq!(size.w.min(), 16.0);
        assert_eq!(size.h.min(), 8.0);
    }

    #[kithara::test]
    fn column_maximizes_width_and_sums_height() {
        let node = ExpandedNode::Column {
            id: None,
            children: vec![
                control("top", fixed(10.0, 4.0)),
                control("bottom", fixed(6.0, 8.0)),
            ],
            size: None,
        };

        let size = compute_size(&node, &EmptyCatalog);

        assert_eq!(size.w.min(), 10.0);
        assert_eq!(size.h.min(), 12.0);
    }

    #[kithara::test]
    fn node_override_wins_over_composed_size() {
        let override_size = fixed(100.0, 50.0);
        let node = ExpandedNode::Row {
            id: None,
            children: vec![control("child", fixed(10.0, 10.0))],
            size: Some(override_size),
        };

        assert_eq!(compute_size(&node, &EmptyCatalog), override_size);
    }

    #[kithara::test]
    fn fill_child_opens_row_width() {
        let node = ExpandedNode::Row {
            id: None,
            children: vec![
                control("fixed", fixed(10.0, 10.0)),
                control("fill", SizeSpec::new(Dim::Fill, Dim::Fixed(10.0))),
            ],
            size: None,
        };

        let size = compute_size(&node, &EmptyCatalog);

        assert_eq!(size.w.min(), 10.0);
        assert_eq!(size.w.max(), None);
    }
}
