use serde::{Deserialize, Serialize};

use super::{
    FontFamily, FontWeight,
    palette::ColorRole,
    primitives::{
        FaceSkin, FrameSkin, ShadowSkin, StateColors, TextRoleSkin, TickSkin, ToneColors,
        WindowControlSkin,
    },
};
use crate::{layout::FrameSides, size::SizeSpec};

/// Hands every frame a value holds to a visitor, however deeply the frames sit.
///
/// A skin declares a frame per control, which is what lets one control differ
/// from the next. Restating all of them to move one number is not what a skin
/// author means by "round the corners", so a patch may name the change once and
/// have it reach every frame through this.
pub(crate) trait Frames {
    fn each_frame(&mut self, visit: &mut dyn FnMut(&mut FrameSkin));
}

/// The same for typographic roles: one face named once reaches every role the
/// document declares.
pub(crate) trait Roles {
    fn each_role(&mut self, visit: &mut dyn FnMut(&mut TextRoleSkin));
}

/// What a skin restates of every frame at once.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct FramePatch {
    pub border: Option<ColorRole>,
    pub border_width: Option<f32>,
    pub radius: Option<f32>,
}

/// What a skin restates of every typographic role at once.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct TextRolePatch {
    pub color: Option<ColorRole>,
    pub font: Option<FontFamily>,
    pub size: Option<f32>,
    pub spacing: Option<f32>,
    pub weight: Option<FontWeight>,
}

impl FramePatch {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn apply(self, frame: &mut FrameSkin) {
        if let Some(border) = self.border {
            frame.border = border;
        }
        if let Some(width) = self.border_width {
            frame.border_width = width;
        }
        if let Some(radius) = self.radius {
            frame.radius = radius;
        }
    }
}

impl TextRolePatch {
    /// Takes every field the patch restates, keeping the rest.
    pub(crate) fn apply(self, role: &mut TextRoleSkin) {
        if let Some(color) = self.color {
            role.color = color;
        }
        if let Some(font) = self.font {
            role.font = font;
        }
        if let Some(size) = self.size {
            role.size = size;
        }
        if let Some(spacing) = self.spacing {
            role.spacing = spacing;
        }
        if let Some(weight) = self.weight {
            role.weight = weight;
        }
    }
}

impl Frames for FrameSkin {
    fn each_frame(&mut self, visit: &mut dyn FnMut(&mut Self)) {
        visit(self);
    }
}

impl Roles for TextRoleSkin {
    fn each_role(&mut self, visit: &mut dyn FnMut(&mut Self)) {
        visit(self);
    }
}

impl<T: Frames> Frames for Option<T> {
    fn each_frame(&mut self, visit: &mut dyn FnMut(&mut FrameSkin)) {
        if let Some(value) = self {
            value.each_frame(visit);
        }
    }
}

impl<T: Roles> Roles for Option<T> {
    fn each_role(&mut self, visit: &mut dyn FnMut(&mut TextRoleSkin)) {
        if let Some(value) = self {
            value.each_role(visit);
        }
    }
}

impl Frames for WindowControlSkin {
    fn each_frame(&mut self, visit: &mut dyn FnMut(&mut FrameSkin)) {
        match self {
            Self::Buttons { .. } => {}
            Self::Close { frame, .. } => frame.each_frame(visit),
        }
    }
}

/// The types a section is built from that hold neither a frame nor a
/// typographic role, and so have nothing to hand a visitor.
macro_rules! plain {
    ($($type:ty,)*) => {$(
        impl Frames for $type {
            fn each_frame(&mut self, _visit: &mut dyn FnMut(&mut FrameSkin)) {}
        }

        impl Roles for $type {
            fn each_role(&mut self, _visit: &mut dyn FnMut(&mut TextRoleSkin)) {}
        }
    )*};
}

plain! {
    bool,
    f32,
    f64,
    u16,
    u32,
    usize,
    ColorRole,
    FaceSkin,
    FontFamily,
    FontWeight,
    FrameSides,
    ShadowSkin,
    SizeSpec,
    StateColors,
    TickSkin,
    ToneColors,
    (f32, ColorRole),
}

impl Roles for FrameSkin {
    fn each_role(&mut self, _visit: &mut dyn FnMut(&mut TextRoleSkin)) {}
}

impl Frames for TextRoleSkin {
    fn each_frame(&mut self, _visit: &mut dyn FnMut(&mut FrameSkin)) {}
}

impl Roles for WindowControlSkin {
    fn each_role(&mut self, _visit: &mut dyn FnMut(&mut TextRoleSkin)) {}
}
