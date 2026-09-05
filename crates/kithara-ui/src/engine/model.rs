use std::ops::Range;

#[cfg(all(test, feature = "masonry"))]
use crate::interact::Gestures;
use crate::interact::{
    CursorShape, Hit, Hover, Outcome, ScrollAxis, TextInputLayout,
    recognizers::{DragEvent, Scalar, Track, WheelStep},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScrollConfig {
    items: Option<ScrollItems>,
    axis: ScrollAxis,
    content_extent: f32,
}

impl ScrollConfig {
    pub(crate) const fn axis(self) -> ScrollAxis {
        self.axis
    }

    pub(crate) const fn content_extent(self) -> f32 {
        self.content_extent
    }

    pub(super) const fn item_layout(self) -> Option<ScrollItems> {
        self.items
    }

    pub(crate) const fn items(
        axis: ScrollAxis,
        content_extent: f32,
        count: usize,
        extent: f32,
        size: f32,
        cross_inset: f32,
    ) -> Self {
        Self {
            axis,
            content_extent,
            items: Some(ScrollItems {
                cross_inset,
                extent,
                size,
                count,
            }),
        }
    }

    pub(crate) const fn plain(axis: ScrollAxis, content_extent: f32) -> Self {
        Self {
            axis,
            content_extent,
            items: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ScrollItems {
    pub(super) cross_inset: f32,
    pub(super) extent: f32,
    pub(super) size: f32,
    pub(super) count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Kind {
    Activation,
    Crossing,
    Segmented,
    Picker,
    TextInput,
    Scroll,
    Item,
    ColumnDivider,
    Fader,
    Crossfader,
    Knob,
    StereoMeter,
    VerticalVu,
    Wave,
    HeroWave,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Identity {
    pub(super) kind: Kind,
    pub(super) path: String,
}

pub(crate) enum Descriptor {
    Activation {
        path: String,
    },
    Crossing {
        path: String,
    },
    Segmented {
        path: String,
        item_count: usize,
    },
    Picker {
        path: String,
        item_count: usize,
        selected: Option<usize>,
    },
    TextInput {
        path: String,
        query: String,
        layout: TextInputLayout,
    },
    Scroll {
        path: String,
        config: ScrollConfig,
    },
    Item {
        target: String,
        path: String,
        count: usize,
    },
    ColumnDivider {
        path: String,
        scalar: Scalar,
        current: f32,
    },
    Fader {
        path: String,
        scalar: Scalar,
        drag_step: Option<f64>,
    },
    Crossfader {
        path: String,
    },
    Knob {
        path: String,
        current: f32,
        drag_range: f32,
        wheel_step: f32,
    },
    StereoMeter {
        path: String,
    },
    VerticalVu {
        path: String,
    },
    Wave {
        path: String,
    },
    HeroWave {
        path: String,
        scale: f32,
        progress: f32,
        visible: Range<f32>,
        wheel_positive: f32,
        wheel_non_positive: f32,
    },
}

impl Descriptor {
    pub(crate) fn activation(path: String) -> Self {
        Self::Activation { path }
    }

    pub(crate) fn column_divider(path: String, value: f32, minimum: f32) -> Self {
        Self::ColumnDivider {
            path,
            current: value,
            scalar: Scalar::builder()
                .track(Track::HorizontalPixels { minimum, value })
                .hover(Hover::new(CursorShape::ResizeH))
                .build(),
        }
    }

    pub(crate) fn crossfader(path: String) -> Self {
        Self::Crossfader { path }
    }

    pub(crate) fn crossing(path: String) -> Self {
        Self::Crossing { path }
    }

    pub(crate) fn fader(
        path: String,
        hover: Hover,
        drag_step: Option<f64>,
        wheel: Option<WheelStep>,
    ) -> Self {
        Self::Fader {
            path,
            drag_step,
            scalar: Scalar::builder()
                .track(Track::AbsoluteHorizontal)
                .hover(hover)
                .maybe_wheel(wheel)
                .build(),
        }
    }

    #[cfg(all(test, feature = "masonry"))]
    pub(crate) const fn gestures(&self) -> Gestures {
        match self {
            Self::Activation { .. } | Self::Segmented { .. } | Self::Wave { .. } => Gestures::PRESS,
            Self::Crossing { .. } => Gestures::empty(),
            Self::Picker { .. } => Gestures::PRESS.union(Gestures::KEYBOARD),
            Self::TextInput { .. } => Gestures::DRAG.union(Gestures::KEYBOARD),
            Self::Scroll { .. } => Gestures::WHEEL,
            Self::Item { .. }
            | Self::ColumnDivider { .. }
            | Self::Crossfader { .. }
            | Self::StereoMeter { .. }
            | Self::VerticalVu { .. } => Gestures::DRAG,
            Self::Fader { scalar, .. } => Gestures::DRAG
                .with(Gestures::DOUBLE_CLICK, scalar.accepts_double_click())
                .with(Gestures::WHEEL, scalar.accepts_wheel()),
            Self::Knob { .. } => Gestures::DRAG
                .union(Gestures::DOUBLE_CLICK)
                .union(Gestures::WHEEL),
            Self::HeroWave { .. } => Gestures::DRAG.union(Gestures::WHEEL),
        }
    }

    pub(crate) fn hero_wave(
        path: String,
        scale: f32,
        progress: f32,
        visible: Range<f32>,
        wheel_positive: f32,
        wheel_non_positive: f32,
    ) -> Self {
        Self::HeroWave {
            path,
            scale,
            visible,
            wheel_positive,
            wheel_non_positive,
            progress: progress.clamp(0.0, 1.0),
        }
    }

    pub(crate) fn item(target: String, path: String, count: usize) -> Self {
        Self::Item {
            target,
            path,
            count,
        }
    }

    pub(super) const fn kind(&self) -> Kind {
        match self {
            Self::Activation { .. } => Kind::Activation,
            Self::Crossing { .. } => Kind::Crossing,
            Self::Segmented { .. } => Kind::Segmented,
            Self::Picker { .. } => Kind::Picker,
            Self::TextInput { .. } => Kind::TextInput,
            Self::Scroll { .. } => Kind::Scroll,
            Self::Item { .. } => Kind::Item,
            Self::ColumnDivider { .. } => Kind::ColumnDivider,
            Self::Fader { .. } => Kind::Fader,
            Self::Crossfader { .. } => Kind::Crossfader,
            Self::Knob { .. } => Kind::Knob,
            Self::StereoMeter { .. } => Kind::StereoMeter,
            Self::VerticalVu { .. } => Kind::VerticalVu,
            Self::Wave { .. } => Kind::Wave,
            Self::HeroWave { .. } => Kind::HeroWave,
        }
    }

    pub(crate) fn knob(path: String, current: f32, drag_range: f32, wheel_step: f32) -> Self {
        Self::Knob {
            path,
            drag_range,
            wheel_step,
            current: current.clamp(0.0, 1.0),
        }
    }

    pub(super) fn path(&self) -> &str {
        match self {
            Self::Activation { path }
            | Self::Crossing { path }
            | Self::Segmented { path, .. }
            | Self::Picker { path, .. }
            | Self::TextInput { path, .. }
            | Self::Scroll { path, .. }
            | Self::ColumnDivider { path, .. }
            | Self::Fader { path, .. }
            | Self::Crossfader { path }
            | Self::Knob { path, .. }
            | Self::StereoMeter { path }
            | Self::VerticalVu { path }
            | Self::Wave { path, .. }
            | Self::HeroWave { path, .. } => path,
            Self::Item { target, .. } => target,
        }
    }

    pub(crate) fn picker(path: String, item_count: usize, selected: Option<usize>) -> Self {
        Self::Picker {
            path,
            item_count,
            selected,
        }
    }

    pub(crate) fn scroll(path: String, config: ScrollConfig) -> Self {
        Self::Scroll { path, config }
    }

    pub(crate) fn segmented(path: String, item_count: usize) -> Self {
        Self::Segmented { path, item_count }
    }

    pub(crate) fn stereo_meter(path: String) -> Self {
        Self::StereoMeter { path }
    }

    pub(crate) fn text_input(path: String, query: String, layout: TextInputLayout) -> Self {
        Self::TextInput {
            path,
            query,
            layout,
        }
    }

    pub(crate) fn vertical_vu(path: String) -> Self {
        Self::VerticalVu { path }
    }

    pub(crate) fn wave(path: String) -> Self {
        Self::Wave { path }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EngineEvent {
    Scalar(f64),
    Activate,
    Crossing(bool),
    Index(usize),
    Drag { event: DragEvent, index: usize },
    Text(String),
}

#[derive(Clone, Copy)]
pub(crate) struct Target<'a> {
    pub(crate) path: &'a str,
    pub(crate) hit: Hit,
    pub(crate) index: Option<usize>,
}

impl<'a> Target<'a> {
    pub(crate) const fn new(path: &'a str, hit: Hit) -> Self {
        Self {
            path,
            hit,
            index: None,
        }
    }

    pub(crate) const fn item(path: &'a str, hit: Hit, index: usize) -> Self {
        Self {
            path,
            hit,
            index: Some(index),
        }
    }
}

pub(crate) struct Emission {
    pub(crate) child: Option<&'static str>,
    pub(crate) outcome: Outcome<EngineEvent>,
    pub(crate) path: String,
}
