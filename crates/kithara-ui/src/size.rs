use serde::{Deserialize, Serialize};

use crate::expand::{ControlSpec, ExpandedNode};

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

struct Consts;

impl Consts {
    const BPM: SizeSpec = SizeSpec::new(Dim::Fixed(64.0), Dim::Fixed(34.0));
    const BRAND: SizeSpec = SizeSpec::new(Dim::Fixed(112.0), Dim::Fixed(34.0));
    const BUTTON: SizeSpec = SizeSpec::new(
        Dim::Range {
            min: 72.0,
            max: None,
        },
        Dim::Fixed(28.0),
    );
    const CHECKBOX: SizeSpec = SizeSpec::new(Dim::Fixed(10.0), Dim::Fixed(10.0));
    const CHIP: SizeSpec = SizeSpec::new(
        Dim::Range {
            min: 24.0,
            max: None,
        },
        Dim::Fixed(18.0),
    );
    const DECK_HEADER: SizeSpec = SizeSpec::new(Dim::Fill, Dim::Fixed(60.0));
    const DECK_SUMMARY: SizeSpec = SizeSpec::new(
        Dim::Range {
            min: 90.0,
            max: None,
        },
        Dim::Fixed(34.0),
    );
    const FADER: SizeSpec = SizeSpec::new(
        Dim::Range {
            min: 90.0,
            max: None,
        },
        Dim::Fixed(34.0),
    );
    const GLOBAL_SPACER: SizeSpec = SizeSpec::new(Dim::Fill, Dim::Fixed(34.0));
    const KNOB: SizeSpec = SizeSpec::new(Dim::Fixed(34.0), Dim::Fixed(34.0));
    const PRESET_SELECTOR: SizeSpec = SizeSpec::new(Dim::Fixed(126.0), Dim::Fixed(34.0));
    const READOUT: SizeSpec = SizeSpec::new(
        Dim::Range {
            min: 40.0,
            max: None,
        },
        Dim::Fixed(26.0),
    );
    const SCALAR: SizeSpec = SizeSpec::new(Dim::Fixed(64.0), Dim::Fixed(18.0));
    const SETTINGS_BUTTON: SizeSpec = SizeSpec::new(Dim::Fixed(34.0), Dim::Fixed(34.0));
    const TEXT: SizeSpec = SizeSpec::new(Dim::Fill, Dim::Fixed(18.0));
    const TIME: SizeSpec = SizeSpec::new(Dim::Fixed(144.0), Dim::Fixed(28.0));
    const TOGGLE: SizeSpec = SizeSpec::new(Dim::Fixed(26.0), Dim::Fixed(14.0));
    const TRACK_LIST: SizeSpec = SizeSpec::new(
        Dim::Range {
            min: 600.0,
            max: None,
        },
        Dim::Range {
            min: 210.0,
            max: None,
        },
    );
    const VU_STEREO: SizeSpec = SizeSpec::new(Dim::Fixed(64.0), Dim::Fixed(22.0));
    const VU_VERTICAL: SizeSpec = SizeSpec::new(Dim::Fixed(18.0), Dim::Fixed(120.0));
    const WAVE: SizeSpec = SizeSpec::new(
        Dim::Fill,
        Dim::Range {
            min: 120.0,
            max: None,
        },
    );
}

/// Returns the intrinsic size for a typed control specification.
#[must_use]
pub fn control_size(spec: &ControlSpec) -> SizeSpec {
    match spec {
        ControlSpec::DeckHeader { .. } => Consts::DECK_HEADER,
        ControlSpec::DeckSummary { .. } => Consts::DECK_SUMMARY,
        ControlSpec::Brand => Consts::BRAND,
        ControlSpec::Spacer => Consts::GLOBAL_SPACER,
        ControlSpec::PresetSelector => Consts::PRESET_SELECTOR,
        ControlSpec::SettingsButton => Consts::SETTINGS_BUTTON,
        ControlSpec::Text { .. } => Consts::TEXT,
        ControlSpec::Button { .. } => Consts::BUTTON,
        ControlSpec::Bpm { .. } => Consts::BPM,
        ControlSpec::Time => Consts::TIME,
        ControlSpec::Scalar { .. } => Consts::SCALAR,
        ControlSpec::Fader { .. } => Consts::FADER,
        ControlSpec::Wave { .. } => Consts::WAVE,
        ControlSpec::TrackList => Consts::TRACK_LIST,
        ControlSpec::Toggle => Consts::TOGGLE,
        ControlSpec::Checkbox => Consts::CHECKBOX,
        ControlSpec::Readout { .. } => Consts::READOUT,
        ControlSpec::Chip { .. } => Consts::CHIP,
        ControlSpec::Knob => Consts::KNOB,
        ControlSpec::VuStereo => Consts::VU_STEREO,
        ControlSpec::VuVertical => Consts::VU_VERTICAL,
    }
}

/// Computes a node's intrinsic size from its override, children, or control specification.
#[must_use]
pub(crate) fn compute_size(node: &ExpandedNode) -> SizeSpec {
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
        ExpandedNode::Row {
            children, gap, pad, ..
        } => inset(
            combine_horizontal(children.iter().map(compute_size)),
            gap_total(*gap, children.len()),
            0.0,
            *pad,
        ),
        ExpandedNode::Column {
            children, gap, pad, ..
        } => inset(
            combine_vertical(children.iter().map(compute_size)),
            0.0,
            gap_total(*gap, children.len()),
            *pad,
        ),
        ExpandedNode::Slot { children, .. } if children.is_empty() => SizeSpec::FILL,
        ExpandedNode::Slot { children, .. } => combine_vertical(children.iter().map(compute_size)),
        ExpandedNode::Control { spec, .. } => control_size(spec),
    }
}

fn gap_total(gap: Option<f32>, child_count: usize) -> f32 {
    let gaps = u16::try_from(child_count.saturating_sub(1)).unwrap_or(u16::MAX);
    gap.unwrap_or(0.0) * f32::from(gaps)
}

fn inset(size: SizeSpec, extra_w: f32, extra_h: f32, pad: Option<f32>) -> SizeSpec {
    let pad = pad.unwrap_or(0.0) * 2.0;
    SizeSpec::new(grow(size.w, extra_w + pad), grow(size.h, extra_h + pad))
}

fn grow(dim: Dim, delta: f32) -> Dim {
    if delta <= 0.0 {
        return dim;
    }
    match dim {
        Dim::Fixed(value) => Dim::Fixed(value + delta),
        Dim::Range { min, max } => Dim::Range {
            min: min + delta,
            max: max.map(|max| max + delta),
        },
        Dim::Fill => Dim::Fill,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        ids::{Interner, SourceUri},
        module::AdaptivePolicy,
    };

    fn control(interner: &mut Interner, id: &str, size: SizeSpec) -> ExpandedNode {
        let origin = SourceUri("size-test.ron".to_owned());
        ExpandedNode::Control {
            path: interner.intern(id, &origin).unwrap(),
            id: interner.intern(id, &origin).unwrap(),
            spec: ControlSpec::Knob,
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
        let mut interner = Interner::new(1024);
        let node = ExpandedNode::Row {
            id: None,
            children: vec![
                control(&mut interner, "left", fixed(10.0, 4.0)),
                control(&mut interner, "right", fixed(6.0, 8.0)),
            ],
            size: None,
            gap: None,
            pad: None,
        };

        let size = compute_size(&node);

        assert_eq!(size.w.min(), 16.0);
        assert_eq!(size.h.min(), 8.0);
    }

    #[kithara::test]
    fn column_maximizes_width_and_sums_height() {
        let mut interner = Interner::new(1024);
        let node = ExpandedNode::Column {
            id: None,
            children: vec![
                control(&mut interner, "top", fixed(10.0, 4.0)),
                control(&mut interner, "bottom", fixed(6.0, 8.0)),
            ],
            size: None,
            gap: None,
            pad: None,
        };

        let size = compute_size(&node);

        assert_eq!(size.w.min(), 10.0);
        assert_eq!(size.h.min(), 12.0);
    }

    #[kithara::test]
    fn node_override_wins_over_composed_size() {
        let mut interner = Interner::new(1024);
        let override_size = fixed(100.0, 50.0);
        let node = ExpandedNode::Row {
            id: None,
            children: vec![control(&mut interner, "child", fixed(10.0, 10.0))],
            size: Some(override_size),
            gap: None,
            pad: None,
        };

        assert_eq!(compute_size(&node), override_size);
    }

    #[kithara::test]
    fn fill_child_opens_row_width() {
        let mut interner = Interner::new(1024);
        let node = ExpandedNode::Row {
            id: None,
            children: vec![
                control(&mut interner, "fixed", fixed(10.0, 10.0)),
                control(
                    &mut interner,
                    "fill",
                    SizeSpec::new(Dim::Fill, Dim::Fixed(10.0)),
                ),
            ],
            size: None,
            gap: None,
            pad: None,
        };

        let size = compute_size(&node);

        assert_eq!(size.w.min(), 10.0);
        assert_eq!(size.w.max(), None);
    }
}
